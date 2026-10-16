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
    let Some(_project_id) = model.workspace.read(cx).project_id.as_ref() else {
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

    let unplanned_approved = forge.snapshot.features.iter().any(|feature| {
        feature.state == hvc_core::feature_forge::FeatureState::Approved && feature.wave.is_none()
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
