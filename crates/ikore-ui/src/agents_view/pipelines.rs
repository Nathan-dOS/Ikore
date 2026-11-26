//! The developer's pipelines on the Agents page (feature 25, M2): their
//! boxes on the canvas, the synthesis a box's title opens, a step's synthesis
//! and the archived pipelines. The editor is `pipeline_editor.rs`.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*};
use ikore_core::custom_pipelines::{
    CustomPipelineView, FindingSeverity, MAX_PROJECT_PIPELINES, PipelineRunStatus, PipelineRunView,
    PipelineStep, PipelineStepRunView, StageVerdictV1, StepRunStatus, StepTransport,
};

use crate::agents::{AgentIdentifier, AgentsModel};
use crate::agents_canvas::{CanvasNode, CanvasPipeline, NodeState};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

use super::{action_button, empty_state, icon_action};

/// `2m ago`, or the time a run has run, from its timestamps.
fn ago(active: bool, started: &str, ended: &str) -> Option<String> {
    crate::agent_dock_view::time_label(active, started, ended, chrono::Utc::now().fixed_offset())
}

/// A run's state and the words for it.
pub(crate) fn run_state(run: &PipelineRunView) -> (NodeState, String) {
    let total = run.definition.steps.len();
    match run.status {
        PipelineRunStatus::Queued => (NodeState::Idle, "Queued".into()),
        PipelineRunStatus::Running => (
            NodeState::Running,
            format!("Running · step {} of {total}", run.current_step + 1),
        ),
        PipelineRunStatus::WaitingGate => (NodeState::Waiting, "Waiting on you".into()),
        PipelineRunStatus::Done => {
            let ended = run.finished_at.as_deref().unwrap_or(&run.updated_at);
            (
                NodeState::Done,
                match ago(false, &run.created_at, ended) {
                    Some(time) => format!("Done · {time}"),
                    None => "Done".into(),
                },
            )
        }
        PipelineRunStatus::Failed => (NodeState::Attention, "Failed".into()),
        PipelineRunStatus::Cancelled => (NodeState::Done, "Cancelled".into()),
        PipelineRunStatus::Interrupted => (NodeState::Attention, "Interrupted".into()),
    }
}

fn step_status_label(step: &PipelineStepRunView) -> &'static str {
    let gate = step.kind == "gate";
    match step.status {
        StepRunStatus::Queued => "Queued",
        StepRunStatus::Running => "Running",
        StepRunStatus::WaitingGate => "Waiting on you",
        StepRunStatus::Passed if gate => "Approved",
        StepRunStatus::Passed => "Passed",
        StepRunStatus::Rejected => "Rejected",
        StepRunStatus::Done => "Done",
        StepRunStatus::Failed => "Failed",
        StepRunStatus::Cancelled => "Cancelled",
        StepRunStatus::Interrupted => "Interrupted",
    }
}

/// A step's node state, from the pipeline's newest run.
fn step_state(run: Option<&PipelineRunView>, index: u32) -> (NodeState, String) {
    let Some(step) = run.and_then(|run| run.step(index)) else {
        return (NodeState::Idle, "Idle".into());
    };
    let label = step_status_label(step);
    match step.status {
        StepRunStatus::Running => {
            let started = step.started_at.as_deref().unwrap_or_default();
            (
                NodeState::Running,
                match ago(true, started, started) {
                    Some(time) => format!("Running {time}"),
                    None => label.into(),
                },
            )
        }
        StepRunStatus::Queued => (NodeState::Idle, label.into()),
        StepRunStatus::WaitingGate => (NodeState::Waiting, label.into()),
        StepRunStatus::Rejected | StepRunStatus::Failed | StepRunStatus::Interrupted => {
            (NodeState::Attention, label.into())
        }
        StepRunStatus::Passed | StepRunStatus::Done | StepRunStatus::Cancelled => {
            (NodeState::Done, label.into())
        }
    }
}

/// `CLI · claude`, `API · {route}`, `Gate` or the host tool.
pub(crate) fn step_transport(model: &AppModel, step: &PipelineStep, cx: &gpui::App) -> String {
    match step {
        PipelineStep::Agent(agent) => match agent.agent.transport {
            StepTransport::Cli => {
                format!("CLI · {}", agent.agent.backend.as_deref().unwrap_or("—"))
            }
            StepTransport::Api => {
                let route = agent.agent.api_route_id.as_deref().unwrap_or("—");
                let name = model
                    .settings
                    .read(cx)
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.routes.iter().find(|item| item.id == route))
                    .map_or(route.to_owned(), |item| item.name.clone());
                format!("API · {name}")
            }
        },
        PipelineStep::Gate(_) => "Gate".into(),
        PipelineStep::HostTool(tool) => tool.tool.as_str().into(),
        PipelineStep::Canonical(canonical) => format!("Canonical · {}", canonical.role.label()),
    }
}

/// The boxes the canvas draws below the default one, in their order.
pub(super) fn canvas_pipelines(model: &AppModel, cx: &gpui::App) -> Vec<CanvasPipeline> {
    let agents = model.agents.read(cx);
    agents
        .pipelines
        .pipelines
        .iter()
        .map(|pipeline| {
            let newest = agents.pipelines.runs_of(&pipeline.id).next();
            let (state, status) = newest.map_or_else(
                || {
                    (
                        NodeState::Idle,
                        ikore_core::custom_pipelines::trigger_label(pipeline.effective_trigger()),
                    )
                },
                run_state,
            );
            let steps = pipeline
                .definition
                .steps
                .iter()
                .enumerate()
                .map(|(index, step)| {
                    // A run in progress follows its own definition (T4); a box
                    // shows the adopted one, so a step it no longer has is idle.
                    let run = newest.filter(|run| {
                        run.definition.steps.get(index).map(PipelineStep::id) == Some(step.id())
                    });
                    let (state, status) = step_state(run, index as u32);
                    CanvasNode {
                        agent: AgentIdentifier::PipelineStep {
                            pipeline_id: pipeline.id.clone(),
                            step_id: step.id().to_owned(),
                        },
                        label: step.label().to_owned(),
                        color: match step {
                            PipelineStep::Agent(_) => theme::accent(),
                            PipelineStep::Gate(_) => theme::warning(),
                            PipelineStep::HostTool(_) => theme::text_muted(),
                            PipelineStep::Canonical(_) => theme::accent(),
                        },
                        state,
                        status,
                        transport: step_transport(model, step, cx),
                        checkpoint: matches!(step, PipelineStep::Gate(_)),
                    }
                })
                .collect();
            CanvasPipeline {
                id: pipeline.id.clone(),
                title: format!(
                    "{}{} · v{} · {}",
                    pipeline.name,
                    if pipeline.is_global() {
                        " · Global"
                    } else {
                        ""
                    },
                    pipeline.adopted_version,
                    if pipeline.ready {
                        status
                    } else {
                        "Needs edit".into()
                    }
                ),
                state,
                steps,
            }
        })
        .collect()
}

/// Whether a new pipeline fits in the project (R4).
pub(super) fn can_create_pipeline(agents: &AgentsModel) -> bool {
    agents
        .pipelines
        .pipelines
        .iter()
        .filter(|pipeline| pipeline.scope == "project")
        .count()
        < MAX_PROJECT_PIPELINES
}

/// `1 step`, `3 steps`.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn heading(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

fn muted(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

fn panel_header(
    selector: &'static str,
    color: gpui::Rgba,
    title: String,
    chip: Option<String>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .items_center()
        .gap_2()
        .child(crate::widgets::status_dot(color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when_some(chip, |row, chip| {
            row.child(crate::widgets::chip(chip, theme::text_muted()))
        })
        .child(icon_action(
            "agents-synthesis-close".into(),
            Icon::X,
            "Close",
            true,
            cx,
            |model, cx| model.close_agents_synthesis(cx),
        ))
        .into_any_element()
}

fn panel(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
}

fn severity_label(severity: FindingSeverity) -> (&'static str, gpui::Rgba) {
    match severity {
        FindingSeverity::Blocking => ("Blocking", theme::danger()),
        FindingSeverity::Warning => ("Warning", theme::warning()),
        FindingSeverity::Note => ("Note", theme::text_muted()),
    }
}

/// A verdict: its summary and its findings, with file and line.
fn verdict_view(verdict: &StageVerdictV1, selector: String) -> AnyElement {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(match verdict.verdict {
                    ikore_core::custom_pipelines::Verdict::Pass => theme::success(),
                    ikore_core::custom_pipelines::Verdict::Reject => theme::danger(),
                })
                .child(format!(
                    "{} · {}",
                    match verdict.verdict {
                        ikore_core::custom_pipelines::Verdict::Pass => "Pass",
                        ikore_core::custom_pipelines::Verdict::Reject => "Reject",
                    },
                    verdict.summary
                )),
        )
        .children(verdict.findings.iter().map(|finding| {
            let (label, color) = severity_label(finding.severity);
            let place = match (&finding.file, finding.line, finding.file_not_found) {
                (Some(file), Some(line), _) => format!(" · {file}:{line}"),
                (Some(file), None, _) => format!(" · {file}"),
                (None, _, true) => " · file not in the workspace".into(),
                (None, _, false) => String::new(),
            };
            div()
                .flex()
                .flex_col()
                .pl_2()
                .border_l_2()
                .border_color(color)
                .child(
                    div()
                        .text_xs()
                        .text_color(color)
                        .child(format!("{label}{place}")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text())
                        .child(finding.message.clone()),
                )
                .when_some(finding.suggestion.clone(), |item, suggestion| {
                    item.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!("Suggestion: {suggestion}")),
                    )
                })
        }))
        .into_any_element()
}

/// A gate the run waits at: its text, an optional note, `Approve` and `Reject`.
fn gate_decision(
    model: &AppModel,
    run: &PipelineRunView,
    index: u32,
    prompt: &str,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let run_id = run.id.clone();
    let reject_run = run.id.clone();
    div()
        .debug_selector(|| "pipeline-gate".into())
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::warning())
        .bg(theme::surface_raised())
        .when(!prompt.trim().is_empty(), |gate| {
            gate.child(div().text_sm().child(prompt.to_owned()))
        })
        .child(super::config::field(
            "NOTE (OPTIONAL)",
            model.pipeline_gate_note_input.clone(),
            crate::sizing::INPUT_MEDIUM,
        ))
        .child(
            div()
                .flex()
                .gap_2()
                .child(action_button(
                    "pipeline-gate-approve".into(),
                    "Approve".into(),
                    !busy,
                    false,
                    cx,
                    move |model, cx| model.decide_pipeline_gate(run_id.clone(), index, true, cx),
                ))
                .child(action_button(
                    "pipeline-gate-reject".into(),
                    "Reject".into(),
                    !busy,
                    true,
                    cx,
                    move |model, cx| {
                        model.decide_pipeline_gate(reject_run.clone(), index, false, cx)
                    },
                )),
        )
        .into_any_element()
}

/// One step of a run: its status, and its output, verdict or gate.
fn run_step(
    model: &AppModel,
    run: &PipelineRunView,
    index: usize,
    step: &PipelineStep,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let state = run.step(index as u32);
    let status = state.map_or("Not started", step_status_label);
    let selector = format!("pipeline-run-step:{}", step.id());
    let mut item = div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .child(div().flex_1().min_w_0().truncate().child(format!(
                    "{}. {}",
                    index + 1,
                    step.label()
                )))
                .child(div().text_xs().text_color(theme::text_muted()).child(
                    match state.map(|state| state.attempt) {
                        Some(attempt) if attempt > 1 => format!("{status} · attempt {attempt}"),
                        _ => status.to_owned(),
                    },
                )),
        );
    let Some(state) = state else {
        return item.into_any_element();
    };
    if let Some(error) = &state.error {
        item = item.child(
            div()
                .p_2()
                .rounded_md()
                .bg(theme::danger_soft())
                .text_xs()
                .text_color(theme::danger())
                .child(error.clone()),
        );
    }
    if let Some(verdict) = &state.verdict {
        item = item.child(verdict_view(
            verdict,
            format!("pipeline-verdict:{}", step.id()),
        ));
    }
    if let Some(op_id) = &state.git_operation_id {
        let short: String = op_id.chars().take(8).collect();
        item = item.child(
            div()
                .debug_selector({
                    let id = step.id().to_owned();
                    move || format!("pipeline-git-op:{id}")
                })
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!("Git operation {short}")),
        );
    }
    if let Some(output) = &state.output_text {
        item = item.child(
            div()
                .debug_selector({
                    let id = step.id().to_owned();
                    move || format!("pipeline-output:{id}")
                })
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .text_xs()
                .text_color(theme::text())
                .child(output.clone()),
        );
    }
    if let Some(note) = &state.gate_feedback {
        item = item.child(muted(format!("Note: {note}")));
    }
    if state.status == StepRunStatus::WaitingGate
        && run.status == PipelineRunStatus::WaitingGate
        && let PipelineStep::Gate(gate) = step
    {
        item = item.child(gate_decision(
            model,
            run,
            index as u32,
            &gate.prompt,
            busy,
            cx,
        ));
    }
    item.into_any_element()
}

/// The run the synthesis shows, with `Cancel` or `Resume` and every step.
fn run_detail(
    model: &AppModel,
    run: &PipelineRunView,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let (state, status) = run_state(run);
    let cancel = run.id.clone();
    let resume = run.id.clone();
    div()
        .debug_selector(|| "pipeline-run-detail".into())
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::widgets::status_dot(match state {
                    NodeState::Running => theme::running(),
                    NodeState::Waiting => theme::warning(),
                    NodeState::Attention => theme::danger(),
                    NodeState::Idle | NodeState::Done => theme::text_muted(),
                }))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("{status} · v{}", run.version)),
                )
                .when(run.status.is_active(), |row| {
                    row.child(action_button(
                        "pipeline-run-cancel".into(),
                        "Cancel".into(),
                        !busy,
                        true,
                        cx,
                        move |model, cx| model.cancel_pipeline_run(cancel.clone(), cx),
                    ))
                })
                .when(run.status == PipelineRunStatus::Interrupted, |row| {
                    row.child(action_button(
                        "pipeline-run-resume".into(),
                        "Resume".into(),
                        !busy,
                        false,
                        cx,
                        move |model, cx| model.resume_pipeline_run(resume.clone(), cx),
                    ))
                }),
        )
        .when_some(run.trigger_label.clone(), |detail, trigger| {
            detail.child(
                div()
                    .debug_selector(|| "pipeline-run-trigger".into())
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .child(div().w_full().min_w_0().child(muted(
                        match &run.source_pulsation_title {
                            Some(title) => format!("Started by {trigger} \u{b7} {title}"),
                            None => format!("Started by {trigger}"),
                        },
                    )))
                    .when(run.source_pulsation_failed, |line| {
                        line.child(
                            div()
                                .debug_selector(|| "pipeline-run-source-failed".into())
                                .child(crate::widgets::chip(
                                    "Source pulsation failed",
                                    theme::warning(),
                                )),
                        )
                    }),
            )
        })
        .when_some(run.error.clone(), |detail, error| {
            detail.child(
                div()
                    .debug_selector(|| "pipeline-run-error".into())
                    .text_xs()
                    .text_color(theme::danger())
                    .child(error),
            )
        })
        .children(
            run.definition
                .steps
                .iter()
                .enumerate()
                .map(|(index, step)| run_step(model, run, index, step, busy, cx))
                .collect::<Vec<_>>(),
        )
        .into_any_element()
}

/// What starts the pipeline. A global pipeline's box follows this project's
/// own trigger when it chose one (R4b, M3): `Trigger here` with `Change`,
/// whose choices save at once, and `Use the pipeline's trigger`.
fn trigger_line(
    pipeline: &CustomPipelineView,
    editing: bool,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    use ikore_core::custom_pipelines::trigger_label;
    if !pipeline.is_global() && pipeline.trigger_override.is_none() {
        return div()
            .debug_selector(|| "agents-pipeline-trigger".into())
            .child(muted(format!(
                "Trigger \u{b7} {}",
                trigger_label(&pipeline.definition.trigger)
            )))
            .into_any_element();
    }
    let effective = pipeline.effective_trigger().clone();
    let own = pipeline.trigger_override.is_some();
    let (toggle, choose, reset) = (
        pipeline.id.clone(),
        pipeline.id.clone(),
        pipeline.id.clone(),
    );
    let text = if own {
        format!(
            "Trigger here \u{b7} {} \u{b7} the pipeline's is {}",
            trigger_label(&effective),
            trigger_label(&pipeline.definition.trigger)
        )
    } else {
        format!(
            "Trigger here \u{b7} {} \u{b7} the pipeline's",
            trigger_label(&effective)
        )
    };
    div()
        .debug_selector(|| "agents-pipeline-trigger".into())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(muted(text)))
                .child(div().flex_none().child(action_button(
                    "pipeline-trigger-change".into(),
                    if editing { "Done" } else { "Change" }.into(),
                    !busy,
                    false,
                    cx,
                    move |model, cx| model.toggle_pipeline_trigger_choices(toggle.clone(), cx),
                ))),
        )
        .when(editing, |line| {
            line.child(super::pipeline_editor::trigger_rows(
                "pipeline-trigger-here",
                &effective,
                false,
                cx,
                move |model, trigger, cx| {
                    model.set_pipeline_trigger(choose.clone(), Some(trigger), cx)
                },
            ))
            .child(muted(
                "Only this project. Other projects keep their own trigger, and editing the pipeline changes it for every project that did not choose one.",
            ))
            .child(action_button(
                "pipeline-trigger-reset".into(),
                "Use the pipeline's trigger".into(),
                !busy && own,
                false,
                cx,
                move |model, cx| model.set_pipeline_trigger(reset.clone(), None, cx),
            ))
        })
        .into_any_element()
}

/// A box's title clicked: trigger, version, the last runs, `Run`, `Edit`,
/// `Archive` and, for a pipeline that never ran, `Delete`.
pub(super) fn pipeline_synthesis(
    model: &AppModel,
    pipeline: &CustomPipelineView,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let runs = agents
        .pipelines
        .runs_of(&pipeline.id)
        .cloned()
        .collect::<Vec<_>>();
    let shown = agents
        .selected_pipeline_run
        .as_ref()
        .and_then(|id| runs.iter().find(|run| &run.id == id))
        .or_else(|| runs.first())
        .cloned();
    let confirming_delete = agents.pending_pipeline_delete.as_ref() == Some(&pipeline.id);
    let editing_trigger = agents.editing_pipeline_trigger.as_ref() == Some(&pipeline.id);
    let active = runs.iter().any(|run| run.status.is_active());
    let (state, _) = runs
        .first()
        .map_or((NodeState::Idle, String::new()), run_state);
    let id = pipeline.id.clone();
    let (run_id, edit_id, archive_id, delete_id) = (id.clone(), id.clone(), id.clone(), id.clone());
    let (child_id, scope_id) = (id.clone(), id.clone());
    let can_create = can_create_pipeline(agents);
    // `Make global` stays off for a child of a project's pipeline (P3).
    let parent_in_project = pipeline.parent_pipeline_id.as_ref().is_some_and(|parent| {
        agents
            .pipeline(parent)
            .is_some_and(|parent| !parent.is_global())
    });
    let scope_actions = if pipeline.is_builtin() {
        vec![(
            "pipeline-remove",
            "Remove from this project",
            crate::agents::PipelineScopeAction::Remove,
            !active,
        )]
    } else if pipeline.is_global() {
        vec![
            (
                "pipeline-move-to-project",
                "Move to this project",
                crate::agents::PipelineScopeAction::MoveToProject,
                pipeline.projects <= 1,
            ),
            (
                "pipeline-remove",
                "Remove from this project",
                crate::agents::PipelineScopeAction::Remove,
                !active,
            ),
        ]
    } else if !parent_in_project {
        vec![(
            "pipeline-make-global",
            "Make global",
            crate::agents::PipelineScopeAction::MakeGlobal,
            true,
        )]
    } else {
        Vec::new()
    };
    let mut badges = Vec::new();
    if pipeline.is_builtin() {
        badges.push("Default".to_owned());
    }
    if pipeline.is_global() {
        badges.push(match pipeline.projects {
            0 | 1 => "Global".to_owned(),
            count => format!("Global \u{b7} {count} projects"),
        });
    }
    if let Some(parent) = super::pipeline_editor::parent_label(pipeline) {
        badges.push(format!("Based on {parent}"));
    }
    let mut scope_buttons = Vec::new();
    for (selector, text, action, enabled) in scope_actions {
        let id = scope_id.clone();
        scope_buttons.push(
            action_button(
                selector.into(),
                text.into(),
                !busy && enabled,
                false,
                cx,
                move |model, cx| model.change_pipeline_scope(id.clone(), action, cx),
            )
            .into_any_element(),
        );
    }
    let mut content = panel("agents-pipeline-synthesis")
        .child(panel_header(
            "agents-pipeline-synthesis-header",
            match state {
                NodeState::Running => theme::running(),
                NodeState::Waiting => theme::warning(),
                NodeState::Attention => theme::danger(),
                NodeState::Idle | NodeState::Done => theme::accent(),
            },
            pipeline.name.clone(),
            Some(format!("v{} \u{b7} adopted", pipeline.adopted_version)),
            cx,
        ))
        .when(!badges.is_empty(), |panel| {
            panel.child(
                div()
                    .debug_selector(|| "agents-pipeline-badges".into())
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(
                        badges
                            .into_iter()
                            .map(|badge| crate::widgets::chip(badge, theme::accent())),
                    ),
            )
        })
        .when(pipeline.behind(), |panel| {
            panel.child(div().debug_selector(|| "agents-pipeline-behind".into()).child(muted(format!(
                "v{} was saved in another project \u{b7} this project takes it when it opens again",
                pipeline.current_version
            ))))
        })
        .when(!pipeline.description.trim().is_empty(), |panel| {
            panel.child(muted(pipeline.description.clone()))
        })
        .child(trigger_line(pipeline, editing_trigger, busy, cx))
        .child(muted(plural(pipeline.definition.steps.len(), "step")))
        .when_some(pipeline.needs_edit_reason.clone(), |panel, reason| {
            panel.child(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!("Needs edit · {reason}")),
            )
        })
        .child(
            div()
                .debug_selector(|| "agents-pipeline-actions".into())
                .flex()
                .flex_wrap()
                .gap_2()
                .child(action_button(
                    "pipeline-run".into(),
                    "Run".into(),
                    !busy && pipeline.ready,
                    false,
                    cx,
                    move |model, cx| model.start_pipeline_run(run_id.clone(), cx),
                ))
                .when(!pipeline.is_builtin(), |row| {
                    row.child(action_button(
                        "pipeline-edit".into(),
                        "Edit".into(),
                        !busy,
                        false,
                        cx,
                        move |model, cx| model.edit_pipeline(edit_id.clone(), cx),
                    ))
                })
                .child(action_button(
                    "pipeline-archive".into(),
                    "Archive".into(),
                    !busy && !active,
                    false,
                    cx,
                    move |model, cx| model.set_pipeline_archived(archive_id.clone(), true, cx),
                ))
                .children(scope_buttons)
                .child(match pipeline.parent_pipeline_id {
                    // One level only (P1): a child is detached, not parented.
                    Some(_) => action_button(
                        "pipeline-detach".into(),
                        "Detach".into(),
                        !busy,
                        false,
                        cx,
                        move |model, cx| {
                            model.change_pipeline_scope(
                                child_id.clone(),
                                crate::agents::PipelineScopeAction::Detach,
                                cx,
                            )
                        },
                    )
                    .into_any_element(),
                    None => action_button(
                        "pipeline-create-child".into(),
                        "Create child".into(),
                        !busy && can_create,
                        false,
                        cx,
                        move |model, cx| model.create_child_pipeline(child_id.clone(), cx),
                    )
                    .into_any_element(),
                })
                .when(pipeline.run_count == 0 && !pipeline.is_builtin(), |row| {
                    row.child(action_button(
                        "pipeline-delete".into(),
                        if confirming_delete {
                            "Confirm delete"
                        } else {
                            "Delete"
                        }
                        .into(),
                        !busy,
                        true,
                        cx,
                        move |model, cx| model.delete_pipeline(delete_id.clone(), cx),
                    ))
                }),
        )
        .child(heading("RUNS"));
    if runs.is_empty() {
        content = content.child(muted("No runs yet. Run starts the steps in order."));
    } else {
        let shown_id = shown.as_ref().map(|run| run.id.clone());
        content = content.child(div().flex().flex_col().gap_1().children(
            runs.iter().enumerate().map(|(index, run)| {
                let (_, status) = run_state(run);
                let selected = shown_id.as_deref() == Some(run.id.as_str());
                let run_id = run.id.clone();
                let started = crate::time::format_timestamp(&run.created_at);
                let selector = format!("pipeline-run-row:{}", index + 1);
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme::accent_soft()))
                    .hover(|style| style.bg(theme::hover_bg()))
                    .on_click(cx.listener(move |model, _, _, cx| {
                        model.agents.update(cx, |agents, _| {
                            agents.selected_pipeline_run = Some(run_id.clone());
                        });
                        cx.notify();
                    }))
                    .child(match &run.trigger_label {
                        Some(trigger) => format!("{status} \u{b7} {started} \u{b7} {trigger}"),
                        None => format!("{status} \u{b7} {started}"),
                    })
            }),
        ));
        if let Some(run) = shown {
            content = content.child(run_detail(model, &run, busy, cx));
        }
    }
    content.into_any_element()
}

/// A step clicked on the canvas: how it runs, its last output and, at a gate
/// the run waits at, the decision.
pub(super) fn step_synthesis(
    model: &AppModel,
    pipeline: &CustomPipelineView,
    step_id: &str,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let Some((index, step)) = pipeline
        .definition
        .steps
        .iter()
        .enumerate()
        .find(|(_, step)| step.id() == step_id)
    else {
        return empty_state("This step is no longer in the pipeline.");
    };
    let agents = model.agents.read(cx);
    let newest = agents
        .pipelines
        .runs_of(&pipeline.id)
        .next()
        .filter(|run| run.definition.steps.get(index).map(PipelineStep::id) == Some(step_id))
        .cloned();
    let pipeline_id = pipeline.id.clone();
    let how = match step {
        PipelineStep::Agent(agent) => {
            let mut line = step_transport(model, step, cx);
            if agent.agent.transport == StepTransport::Cli {
                line.push_str(" · ");
                line.push_str(agent.agent.model.as_deref().unwrap_or("no model"));
                if agent.agent.thinking_level != "default" {
                    line.push_str(" · ");
                    line.push_str(&agent.agent.thinking_level);
                }
            }
            line.push_str(match agent.output {
                ikore_core::custom_pipelines::StepOutput::Text => " · Text",
                ikore_core::custom_pipelines::StepOutput::Verdict => " · Verdict",
            });
            line
        }
        PipelineStep::Gate(_) => "Gate · a human approval".into(),
        PipelineStep::HostTool(tool) => format!("Host tool · {}", tool.tool.as_str()),
        PipelineStep::Canonical(canonical) => {
            format!("Canonical role · {}", canonical.role.label())
        }
    };
    let instructions = match step {
        PipelineStep::Agent(agent) => agent.instructions.clone(),
        PipelineStep::Gate(gate) => gate.prompt.clone(),
        PipelineStep::HostTool(_) => String::new(),
        PipelineStep::Canonical(canonical) => canonical.instructions_append.clone(),
    };
    panel("agents-step-synthesis")
        .child(panel_header(
            "agents-step-synthesis-header",
            match step {
                PipelineStep::Gate(_) => theme::warning(),
                _ => theme::accent(),
            },
            step.label().to_owned(),
            Some(format!(
                "Step {} of {}",
                index + 1,
                pipeline.definition.steps.len()
            )),
            cx,
        ))
        .child(
            div()
                .id("agents-step-pipeline")
                .debug_selector(|| "agents-step-pipeline".into())
                .text_xs()
                .text_color(theme::accent())
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.select_canvas_pipeline(pipeline_id.clone(), cx);
                }))
                .child(format!("{} · v{}", pipeline.name, pipeline.adopted_version)),
        )
        .child(muted(how))
        .when(!instructions.trim().is_empty(), |panel| {
            panel
                .child(heading(match step {
                    PipelineStep::Gate(_) => "TEXT",
                    _ => "INSTRUCTIONS",
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text())
                        .child(instructions),
                )
        })
        .child(heading("LAST RUN"))
        .child(match newest {
            Some(run) => run_step(model, &run, index, step, busy, cx),
            None => muted("Not run yet."),
        })
        .into_any_element()
}

/// `Add global pipeline…`: the global pipelines this project does not show,
/// each with `Add`.
pub(super) fn global_pipelines(
    model: &AppModel,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let globals = model.agents.read(cx).pipelines.available_globals.clone();
    let count = globals.len();
    panel("agents-drawer:global-pipelines")
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Global pipelines \u{b7} {count}")),
                )
                .child(icon_action(
                    "agents-drawer-close".into(),
                    Icon::X,
                    "Close",
                    true,
                    cx,
                    |model, cx| model.close_agents_synthesis(cx),
                )),
        )
        .child(muted(
            "A global pipeline added here gets a box in this project. Edits made in any project reach this one when it opens.",
        ))
        .when(count == 0, |panel| {
            panel.child(empty_state("Every global pipeline is already here."))
        })
        .children(globals.into_iter().map(|global| {
            let add = global.id.clone();
            let selector = format!("global-pipeline:{}", global.id);
            div()
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(global.name.clone()),
                        )
                        .when(global.is_builtin, |row| {
                            row.child(crate::widgets::chip("Default", theme::accent()))
                        }),
                )
                .child(muted(match &global.parent_name {
                    Some(parent) => format!("v{} \u{b7} Based on {parent}", global.current_version),
                    None => format!("v{}", global.current_version),
                }))
                .when(!global.description.trim().is_empty(), |card| {
                    card.child(muted(global.description.clone()))
                })
                .child(div().flex().child(action_button(
                    "pipeline-add-global".into(),
                    "Add".into(),
                    !busy,
                    false,
                    cx,
                    move |model, cx| {
                        model.change_pipeline_scope(
                            add.clone(),
                            crate::agents::PipelineScopeAction::Add,
                            cx,
                        )
                    },
                )))
                .into_any_element()
        }))
        .into_any_element()
}

/// `Archived pipelines · N`: each with `Restore` and, without runs, `Delete`.
pub(super) fn archived_pipelines(
    model: &AppModel,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let archived = agents.pipelines.archived.clone();
    let confirming = agents.pending_pipeline_delete.clone();
    let count = archived.len();
    panel("agents-drawer:archived-pipelines")
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Archived pipelines · {count}")),
                )
                .child(icon_action(
                    "agents-drawer-close".into(),
                    Icon::X,
                    "Close",
                    true,
                    cx,
                    |model, cx| model.close_agents_synthesis(cx),
                )),
        )
        .when(count == 0, |panel| {
            panel.child(empty_state("No archived pipelines."))
        })
        .children(archived.into_iter().map(|pipeline| {
            let restore = pipeline.id.clone();
            let delete = pipeline.id.clone();
            let confirming = confirming.as_ref() == Some(&pipeline.id);
            let selector = format!("archived-pipeline:{}", pipeline.id);
            div()
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(pipeline.name.clone()),
                )
                .child(muted(format!(
                    "v{} · {}",
                    pipeline.adopted_version,
                    plural(pipeline.run_count as usize, "run")
                )))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "pipeline-restore".into(),
                            "Restore".into(),
                            !busy,
                            false,
                            cx,
                            move |model, cx| {
                                model.set_pipeline_archived(restore.clone(), false, cx)
                            },
                        ))
                        .when(pipeline.run_count == 0, |row| {
                            row.child(action_button(
                                "pipeline-delete".into(),
                                if confirming {
                                    "Confirm delete"
                                } else {
                                    "Delete"
                                }
                                .into(),
                                !busy,
                                true,
                                cx,
                                move |model, cx| model.delete_pipeline(delete.clone(), cx),
                            ))
                        }),
                )
                .into_any_element()
        }))
        .into_any_element()
}
