//! Feature 25 M1: the UI latency probe's canvas workload
//! (`agents-canvas-physics`). The Agents canvas with twenty nodes, three runs
//! streaming as in the dock workload, and the Code stage dragged around by
//! script and let go now and then, so the bubble physics, the pulses and
//! the dock all work at once. In memory only: no CLI, repository or
//! credential is touched.

use std::time::Instant;

use ikore_core::llm::AgentRole;
use ikore_core::terminals::{FreeFormTerminalSnapshot, TerminalsSnapshot};

use crate::agents::AgentsDashboardSnapshot;
use crate::performance_agent_dock::{PROJECT, ROLES, agent, envelope, free_form, pulse};

/// Every role, three of them running, and nine free-form agents: twenty
/// nodes. The page offers a ninth free-form agent to nobody, but a snapshot
/// may carry it, and the canvas lays it out like the others.
const FREE_FORMS: usize = 9;

/// The node the script drags: a stage, so its neighbours answer on the links.
const DRAGGED: &str = "role:backend_agent";

/// Frames between two releases: each release sets the outline swaying.
const GRIP_FRAMES: u64 = 180;

fn snapshot() -> TerminalsSnapshot {
    TerminalsSnapshot {
        project_id: PROJECT.into(),
        agents: AgentRole::ALL
            .into_iter()
            .map(|role| {
                let mut snapshot = agent(role);
                if !ROLES.contains(&role) {
                    snapshot.recent_runs.clear();
                }
                snapshot
            })
            .collect(),
        free_form_slots: (0..FREE_FORMS)
            .map(free_form)
            .collect::<Vec<FreeFormTerminalSnapshot>>(),
        archived_free_form_slots: vec![],
        discovery: vec![],
    }
}

/// Frame 0 opens the Agents canvas on the fixture; every frame, each of the
/// three runs gets an event and a pulse, and the dragged node moves along a
/// figure eight. Returns the runs updated.
pub(crate) fn apply(
    model: &mut crate::AppModel,
    frame: u64,
    cx: &mut gpui::Context<crate::AppModel>,
) -> usize {
    if frame == 0 {
        model.agents.update(cx, |agents, _| {
            let generation = agents.begin(true);
            agents.apply_snapshot(
                generation,
                AgentsDashboardSnapshot {
                    terminals: snapshot(),
                    fallbacks: vec![],
                    free_form_bindings: vec![],
                    api_executions: Default::default(),
                    quota_deltas: Default::default(),
                    live_activity: Vec::new(),
                },
            );
        });
        model.show_view_for_benchmark(crate::app_model::AppView::Agents, cx);
        model.agent_dock.mode = Some(crate::agent_dock::DockMode::Expanded);
    }
    let now = Instant::now();
    for role in ROLES {
        let envelope = envelope(role, frame);
        model.note_agent_dock_run(&envelope, cx);
        model.agents.update(cx, |agents, _| {
            agents.record_event(envelope);
            agents.live.record(&pulse(role, frame), now);
        });
    }
    if frame % GRIP_FRAMES == GRIP_FRAMES - 1 {
        model.agents_canvas.script_release();
    } else {
        let t = frame as f32 * 0.05;
        model
            .agents_canvas
            .script_drag(DRAGGED, (120.0 * t.sin(), 60.0 * (2.0 * t).sin()));
    }
    cx.notify();
    ROLES.len()
}
