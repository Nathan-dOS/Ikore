//! The bottom agent dock (ADR-040, M3 and M4; specification 9 to 11): the
//! console of the agent at work under every view except Agents, as a 32 px
//! strip or an expanded panel, following the agent that started last.
//!
//! The state lives here; `agent_dock_view` renders it. Mode, height, the
//! follow preference and the agent shown are kept per project with the UI
//! session.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gpui::{Context, Pixels, Task, px};
use ikore_core::llm::AgentRole;
use ikore_core::persistence_service::AgentDockSession;
use ikore_core::terminals::{TerminalEventEnvelope, TerminalRunStatus};

use crate::agent_console::follow::{FollowInput, FollowState, Priority, TICK};
use crate::agents::AgentIdentifier;
use crate::app_model::{AppModel, AppView};

/// The strip, and the expanded dock while no run has appeared.
pub(crate) const STRIP_HEIGHT: Pixels = px(32.0);
/// The sash above the expanded dock.
pub(crate) const SASH_HEIGHT: Pixels = px(6.0);
pub(crate) const DEFAULT_HEIGHT: f32 = 240.0;
pub(crate) const MIN_HEIGHT: f32 = 160.0;
/// The view above the dock keeps at least this much.
pub(crate) const FRAME_MIN_HEIGHT: f32 = 240.0;
/// The sash (or the strip's top gap), 6 px, and the gap to the status bar, 8 px.
const DOCK_GAPS: f32 = 14.0;
/// From this `shell-body` height a project starts with the dock expanded.
const EXPANDED_FROM_BODY: f32 = 700.0;
/// Tabs shown before `+{n}`.
pub(crate) const VISIBLE_TABS: usize = 6;
/// The console surface the dock renders.
pub(crate) const DOCK_SURFACE: &str = "dock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockMode {
    Hidden,
    Strip,
    Expanded,
}

impl DockMode {
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Strip => "strip",
            Self::Expanded => "expanded",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "hidden" => Some(Self::Hidden),
            "strip" => Some(Self::Strip),
            "expanded" => Some(Self::Expanded),
            _ => None,
        }
    }
}

/// `role:backend_agent` or `slot:<id>`, as the session keeps the agent.
pub(crate) fn agent_key(agent: &AgentIdentifier) -> String {
    match agent {
        AgentIdentifier::Canonical(role) => format!("role:{}", role.as_str()),
        AgentIdentifier::FreeForm(slot) => format!("slot:{slot}"),
        AgentIdentifier::PipelineStep {
            pipeline_id,
            step_id,
        } => format!("pipeline:{pipeline_id}/{step_id}"),
        AgentIdentifier::StageVerifier(id) => format!("verifier:{id}"),
    }
}

pub(crate) fn agent_from_key(key: &str) -> Option<AgentIdentifier> {
    if let Some(role) = key.strip_prefix("role:") {
        return AgentRole::parse(role).map(AgentIdentifier::Canonical);
    }
    if let Some(step) = key.strip_prefix("pipeline:") {
        let (pipeline, step) = step.split_once('/')?;
        return (!pipeline.is_empty() && !step.is_empty()).then(|| AgentIdentifier::PipelineStep {
            pipeline_id: pipeline.to_owned(),
            step_id: step.to_owned(),
        });
    }
    if let Some(id) = key.strip_prefix("verifier:") {
        return (!id.is_empty()).then(|| AgentIdentifier::StageVerifier(id.to_owned()));
    }
    key.strip_prefix("slot:")
        .filter(|slot| !slot.is_empty())
        .map(|slot| AgentIdentifier::FreeForm(slot.to_owned()))
}

/// The pipeline stages in their order, then the support agents (12.1).
const PIPELINE: [AgentRole; 6] = [
    AgentRole::IntakeRouter,
    AgentRole::TaskSpecifier,
    AgentRole::ContractAgent,
    AgentRole::BackendAgent,
    AgentRole::TestAgent,
    AgentRole::Integrator,
];

/// A pipeline stage's step, for `Code · BackendAgent`.
pub(crate) fn pipeline_step(role: AgentRole) -> Option<&'static str> {
    Some(match role {
        AgentRole::IntakeRouter => "Intake",
        AgentRole::TaskSpecifier => "Spec",
        AgentRole::ContractAgent => "Contract",
        AgentRole::BackendAgent => "Code",
        AgentRole::TestAgent => "Test",
        AgentRole::Integrator => "Integrator",
        _ => return None,
    })
}

/// Stable tab order (feature 25, section 6 & M4): each default stage followed
/// immediately by its stage verifiers in position order, then support agents,
/// then the pipelines' steps in the order of their boxes and steps, then free-form
/// slots in the order they appeared (9.3).
fn tab_rank(
    agent: &AgentIdentifier,
    arrival: usize,
    verifiers: &HashMap<AgentIdentifier, (usize, usize)>,
    steps: &HashMap<AgentIdentifier, usize>,
) -> (u8, usize, usize) {
    match agent {
        AgentIdentifier::Canonical(role) => match PIPELINE.iter().position(|stage| stage == role) {
            Some(position) => (0, position, 0),
            None => (
                1,
                AgentRole::ALL
                    .iter()
                    .position(|candidate| candidate == role)
                    .unwrap_or(usize::MAX),
                0,
            ),
        },
        AgentIdentifier::StageVerifier(_) => match verifiers.get(agent).copied() {
            Some((stage_index, pos)) => (0, stage_index, 1 + pos),
            None => (0, usize::MAX, 1 + arrival),
        },
        AgentIdentifier::PipelineStep { .. } => (
            2,
            steps
                .get(agent)
                .copied()
                .unwrap_or(usize::MAX / 2 + arrival),
            0,
        ),
        AgentIdentifier::FreeForm(_) => (3, arrival, 0),
    }
}

#[derive(Debug)]
pub(crate) struct AgentDock {
    /// `None` until the developer picks one: the project default (9.2).
    pub(crate) mode: Option<DockMode>,
    /// The visible mode `Ctrl+J` brings back.
    pub(crate) last_visible: Option<DockMode>,
    /// `None`: the default height.
    pub(crate) height: Option<f32>,
    pub(crate) follow: FollowState,
    /// Agents with a run in this session, in the order they appeared.
    tabs: Vec<AgentIdentifier>,
    /// Tabs the developer closed, until the agent's next run.
    closed: HashSet<AgentIdentifier>,
    /// The run each agent last started, to tell a new one.
    runs: HashMap<AgentIdentifier, String>,
    /// Each stage verifier's `(stage_column, position)` for immediate post-stage tab order.
    verifier_order: HashMap<AgentIdentifier, (usize, usize)>,
    /// Each pipeline step's place: its box, then its step (feature 25).
    step_order: HashMap<AgentIdentifier, usize>,
    /// The run a History row or a unit chose for its agent (feature 25,
    /// R13). It holds while the dock is pinned on that agent; the session
    /// does not keep it.
    pub(crate) chosen_run: Option<(AgentIdentifier, String)>,
    /// The sash drag: where it started and the height then.
    pub(crate) drag: Option<(Pixels, f32)>,
    /// The height the dock took on the last frame, sash included.
    pub(crate) rendered: Pixels,
    tick: Option<Task<()>>,
}

impl Default for AgentDock {
    fn default() -> Self {
        Self {
            mode: None,
            last_visible: None,
            height: None,
            follow: FollowState::default(),
            tabs: Vec::new(),
            closed: HashSet::new(),
            runs: HashMap::new(),
            verifier_order: HashMap::new(),
            step_order: HashMap::new(),
            chosen_run: None,
            drag: None,
            rendered: px(0.0),
            tick: None,
        }
    }
}

impl AgentDock {
    /// The mode on screen: the project's, or its default for this window.
    pub(crate) fn effective_mode(&self, body: Pixels) -> DockMode {
        self.mode
            .unwrap_or(if f32::from(body) >= EXPANDED_FROM_BODY {
                DockMode::Expanded
            } else {
                DockMode::Strip
            })
    }

    /// The tallest the expanded dock may be: 60 % of the body, and the view
    /// above keeps 240 px.
    pub(crate) fn max_height(body: Pixels) -> f32 {
        let body = f32::from(body);
        // The sash and the gap to the status bar come out of the body too.
        (body * 0.6)
            .min(body - FRAME_MIN_HEIGHT - DOCK_GAPS)
            .max(MIN_HEIGHT)
    }

    /// The expanded dock's height, inside its limits.
    pub(crate) fn expanded_height(&self, body: Pixels) -> f32 {
        self.height
            .unwrap_or(DEFAULT_HEIGHT)
            .clamp(MIN_HEIGHT, Self::max_height(body))
    }

    /// Whether any run appeared in this session.
    pub(crate) fn has_runs(&self) -> bool {
        !self.tabs.is_empty()
    }

    /// The tabs to show, in their stable order.
    pub(crate) fn tabs(&self) -> Vec<AgentIdentifier> {
        let mut tabs = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, agent)| !self.closed.contains(*agent))
            .map(|(arrival, agent)| {
                (
                    tab_rank(agent, arrival, &self.verifier_order, &self.step_order),
                    agent.clone(),
                )
            })
            .collect::<Vec<_>>();
        tabs.sort_by_key(|(rank, _)| *rank);
        tabs.into_iter().map(|(_, agent)| agent).collect()
    }

    /// The agent the dock shows: the one it follows, or the first tab.
    pub(crate) fn shown(&self) -> Option<AgentIdentifier> {
        self.follow
            .current
            .clone()
            .filter(|agent| !self.closed.contains(agent))
            .or_else(|| self.tabs().into_iter().next())
    }

    /// A run of `agent` changed; returns whether the shown agent changed.
    pub(crate) fn note_run(
        &mut self,
        agent: AgentIdentifier,
        run_id: &str,
        status: TerminalRunStatus,
        priority: Priority,
        now: Instant,
    ) -> bool {
        if !self.tabs.contains(&agent) {
            self.tabs.push(agent.clone());
        }
        let new_run = self.runs.get(&agent).is_none_or(|known| known != run_id);
        if status.is_active() && new_run {
            self.runs.insert(agent.clone(), run_id.to_owned());
            // A closed tab comes back with the agent's next run.
            self.closed.remove(&agent);
            return self.follow.apply(FollowInput::RunActivated {
                agent,
                run_id: run_id.to_owned(),
                priority,
                at: now,
            });
        }
        if status.is_terminal() {
            return self.follow.apply(FollowInput::RunEnded {
                agent,
                run_id: run_id.to_owned(),
                at: now,
            });
        }
        false
    }

    pub(crate) fn close_tab(&mut self, agent: &AgentIdentifier) {
        self.closed.insert(agent.clone());
        if self
            .chosen_run
            .as_ref()
            .is_some_and(|(chosen, _)| chosen == agent)
        {
            self.chosen_run = None;
        }
    }

    /// Stage verifiers in `stage_column x position` order (feature 25, M4).
    pub(crate) fn set_verifier_order(
        &mut self,
        verifiers: impl IntoIterator<Item = (AgentIdentifier, usize, usize)>,
    ) {
        self.verifier_order = verifiers
            .into_iter()
            .enumerate()
            .map(|(fallback_pos, (agent, stage_index, pos))| {
                (
                    agent,
                    (stage_index, pos.saturating_mul(1024) + fallback_pos),
                )
            })
            .collect();
    }

    /// The pipelines' steps in canvas order: boxes, then steps.
    pub(crate) fn set_step_order(&mut self, steps: impl IntoIterator<Item = AgentIdentifier>) {
        self.step_order = steps
            .into_iter()
            .enumerate()
            .map(|(place, step)| (step, place))
            .collect();
    }

    /// An agent the developer asked for gets a tab, run or not.
    pub(crate) fn reveal(&mut self, agent: &AgentIdentifier) {
        if !self.tabs.contains(agent) {
            self.tabs.push(agent.clone());
        }
        self.closed.remove(agent);
    }

    /// The run the dock shows for `agent`: the one chosen, while the dock
    /// is pinned on the agent; otherwise none, and the latest shows.
    pub(crate) fn chosen_run_of(&self, agent: &AgentIdentifier) -> Option<&str> {
        let pinned = matches!(
            &self.follow.mode,
            crate::agent_console::follow::FollowMode::Pinned(pinned) if pinned == agent
        );
        self.chosen_run
            .as_ref()
            .filter(|(chosen, _)| pinned && chosen == agent)
            .map(|(_, run)| run.as_str())
    }

    pub(crate) fn session(&self) -> AgentDockSession {
        AgentDockSession {
            mode: self.mode.map(|mode| mode.id().to_owned()),
            height: self.height.map(f64::from),
            follow: self.follow.enabled,
            agent: self.shown().as_ref().map(agent_key),
        }
    }

    /// The project's dock as its session left it; defaults where it said
    /// nothing. The tabs start empty: they belong to the session's runs.
    pub(crate) fn restore(session: Option<&AgentDockSession>) -> Self {
        let mut dock = Self::default();
        let Some(session) = session else {
            return dock;
        };
        dock.mode = session.mode.as_deref().and_then(DockMode::from_id);
        dock.last_visible = dock.mode.filter(|mode| *mode != DockMode::Hidden);
        dock.height = session
            .height
            .map(|height| height as f32)
            .filter(|height| height.is_finite());
        dock.follow = FollowState::new(session.follow);
        dock.follow.current = session.agent.as_deref().and_then(agent_from_key);
        dock
    }
}

impl AppModel {
    /// The shell body's height: the window less header and status bar.
    pub(crate) fn shell_body_height(window: &gpui::Window) -> Pixels {
        window.viewport_size().height
            - crate::sizing::HEADER_HEIGHT
            - crate::sizing::STATUS_BAR_HEIGHT
    }

    /// Whether the dock shows: under every view, the Agents canvas too
    /// (feature 25, M1; it replaces the N4 rule of ADR-040).
    pub(crate) fn agent_dock_visible(&self, body: Pixels) -> bool {
        self.agent_dock.effective_mode(body) != DockMode::Hidden
    }

    /// What the Companion keeps clear above the status bar (9.6).
    pub(crate) fn agent_dock_inset(&self) -> Pixels {
        self.agent_dock.rendered
    }

    /// `Ctrl+J` and the status bar item: hidden, or back to the last visible
    /// mode. Opening never moves the focus.
    pub(crate) fn toggle_agent_dock(&mut self, body: Pixels, cx: &mut Context<Self>) {
        let current = self.agent_dock.effective_mode(body);
        let next = if current == DockMode::Hidden {
            self.agent_dock
                .last_visible
                .unwrap_or(if f32::from(body) >= EXPANDED_FROM_BODY {
                    DockMode::Expanded
                } else {
                    DockMode::Strip
                })
        } else {
            DockMode::Hidden
        };
        self.set_agent_dock_mode(next, cx);
    }

    pub(crate) fn set_agent_dock_mode(&mut self, mode: DockMode, cx: &mut Context<Self>) {
        if mode != DockMode::Hidden {
            self.agent_dock.last_visible = Some(mode);
        }
        if self.agent_dock.mode != Some(mode) {
            self.agent_dock.mode = Some(mode);
            self.schedule_session_persistence(cx);
        }
        cx.notify();
    }

    /// A tab click: the dock stays on that agent until `Resume following`.
    /// On the Agents page the agent's node is selected too (feature 25, M1).
    pub(crate) fn pin_agent_dock(&mut self, agent: AgentIdentifier, cx: &mut Context<Self>) {
        if self.active_view() == AppView::Agents {
            self.select_canvas_agent(agent, cx);
            return;
        }
        self.agent_dock.chosen_run = None;
        self.agent_dock.follow.apply(FollowInput::Pin(agent));
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    /// A node clicked, a History row or a unit (feature 25, M1): the dock
    /// pins on the agent and, with `run`, on that run (R13). A hidden dock
    /// opens in its last visible mode, or the project default (R14).
    pub(crate) fn focus_agent_in_dock(
        &mut self,
        agent: AgentIdentifier,
        run: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.agent_dock.reveal(&agent);
        self.agent_dock.chosen_run = run.map(|run| (agent.clone(), run));
        self.agent_dock.follow.apply(FollowInput::Pin(agent));
        if self.agent_dock.mode == Some(DockMode::Hidden) {
            self.agent_dock.mode = self.agent_dock.last_visible;
        }
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    /// The chip's `Latest`: back to the agent's latest run.
    pub(crate) fn show_latest_agent_run(&mut self, cx: &mut Context<Self>) {
        self.agent_dock.chosen_run = None;
        cx.notify();
    }

    pub(crate) fn resume_agent_dock_following(&mut self, cx: &mut Context<Self>) {
        self.agent_dock.chosen_run = None;
        if self.agent_dock.follow.apply(FollowInput::ResumeFollowing) {
            self.schedule_session_persistence(cx);
        }
        cx.notify();
    }

    /// The `Switch to {agent} →` chip: the held-back switch, now.
    pub(crate) fn take_agent_dock_switch(&mut self, cx: &mut Context<Self>) {
        let Some((agent, _)) = self.agent_dock.follow.pending.clone() else {
            return;
        };
        self.agent_dock.follow.current = Some(agent);
        self.agent_dock.follow.pending = None;
        self.agent_dock.follow.guard_until = None;
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    pub(crate) fn close_agent_dock_tab(&mut self, agent: AgentIdentifier, cx: &mut Context<Self>) {
        self.agent_dock.close_tab(&agent);
        cx.notify();
    }

    /// The pointer, the wheel or a key in the dock: no switch for a moment.
    pub(crate) fn note_agent_dock_interaction(&mut self, cx: &mut Context<Self>) {
        self.agent_dock
            .follow
            .apply(FollowInput::Interaction { at: Instant::now() });
        self.ensure_agent_dock_tick(cx);
    }

    /// Settings › Agent console › Follow the active agent.
    pub(crate) fn set_agent_dock_follow(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.agent_dock.follow.apply(FollowInput::SetEnabled {
            enabled,
            at: Instant::now(),
        });
        self.ensure_agent_dock_tick(cx);
        self.schedule_session_persistence(cx);
        cx.notify();
    }

    /// A terminal envelope: the run's agent gets its tab, and following
    /// learns that a run started or ended.
    pub(crate) fn note_agent_dock_run(
        &mut self,
        envelope: &TerminalEventEnvelope,
        cx: &mut Context<Self>,
    ) {
        let agent = AgentIdentifier::of_run(&envelope.run);
        let changed = self.agent_dock.note_run(
            agent,
            &envelope.run_id,
            envelope.run.status,
            Priority::of(envelope.run.origin),
            Instant::now(),
        );
        if changed {
            self.schedule_session_persistence(cx);
        }
        self.ensure_agent_dock_tick(cx);
    }

    /// A tick every 250 ms while a switch waits for its time (10.2).
    pub(crate) fn ensure_agent_dock_tick(&mut self, cx: &mut Context<Self>) {
        if !self.agent_dock.follow.needs_tick() || self.agent_dock.tick.is_some() {
            return;
        }
        self.agent_dock.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let keep = this
                    .update(cx, |model, cx| {
                        let changed = model
                            .agent_dock
                            .follow
                            .apply(FollowInput::Tick(Instant::now()));
                        if changed {
                            model.schedule_session_persistence(cx);
                        }
                        cx.notify();
                        let keep = model.agent_dock.follow.needs_tick();
                        if !keep {
                            model.agent_dock.tick = None;
                        }
                        keep
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    /// The sash: dragging sets the height, saved when the drag ends.
    pub(crate) fn drag_agent_dock(&mut self, y: Pixels, body: Pixels, cx: &mut Context<Self>) {
        let Some((start, height)) = self.agent_dock.drag else {
            return;
        };
        let next = (height + f32::from(start - y)).clamp(MIN_HEIGHT, AgentDock::max_height(body));
        if self.agent_dock.height != Some(next) {
            self.agent_dock.height = Some(next);
            cx.notify();
        }
    }

    pub(crate) fn end_agent_dock_drag(&mut self, cx: &mut Context<Self>) {
        if self.agent_dock.drag.take().is_some() {
            self.schedule_session_persistence(cx);
        }
    }

    /// A double click on the sash: the default height, or the tallest.
    pub(crate) fn toggle_agent_dock_height(&mut self, body: Pixels, cx: &mut Context<Self>) {
        let max = AgentDock::max_height(body);
        let current = self.agent_dock.expanded_height(body);
        self.agent_dock.height = Some(if (current - max).abs() < 1.0 {
            DEFAULT_HEIGHT.min(max)
        } else {
            max
        });
        self.schedule_session_persistence(cx);
        cx.notify();
    }
}

impl AppModel {
    /// An API route of an agent started or ended: the dock follows it as it
    /// follows a run (ADR-040, vector 10).
    pub(crate) fn note_agent_dock_route(
        &mut self,
        envelope: &ikore_core::llm::LlmEventEnvelope,
        cx: &mut Context<Self>,
    ) {
        use ikore_core::llm::LlmStreamEvent;
        let Some(context) = envelope.context.as_ref() else {
            return;
        };
        let status = match envelope.event {
            LlmStreamEvent::RouteStarted { .. } => TerminalRunStatus::Running,
            LlmStreamEvent::RouteCompleted { .. } => TerminalRunStatus::Completed,
            LlmStreamEvent::RouteFailed { .. } => TerminalRunStatus::Failed,
            _ => return,
        };
        if self
            .agents
            .read(cx)
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.project_id != context.project_id)
        {
            return;
        }
        let priority = if context.pulsation_id.is_some() {
            Priority::Pipeline
        } else {
            Priority::Interactive
        };
        let changed = self.agent_dock.note_run(
            AgentIdentifier::from_target(&context.target),
            &envelope.execution_id,
            status,
            priority,
            Instant::now(),
        );
        if changed {
            self.schedule_session_persistence(cx);
        }
        self.ensure_agent_dock_tick(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_default_depends_on_the_body_height() {
        let dock = AgentDock::default();
        assert_eq!(dock.effective_mode(px(568.0)), DockMode::Strip);
        assert_eq!(dock.effective_mode(px(748.0)), DockMode::Expanded);
        // 60 % of the body, and 240 px for the view above.
        assert_eq!(AgentDock::max_height(px(568.0)), 314.0);
        assert!((AgentDock::max_height(px(1368.0)) - 820.8).abs() < 0.01);
        assert_eq!(dock.expanded_height(px(568.0)), DEFAULT_HEIGHT);
    }

    #[test]
    fn tabs_keep_the_pipeline_order_and_a_closed_tab_returns_with_the_next_run() {
        let mut dock = AgentDock::default();
        let now = Instant::now();
        let slot = AgentIdentifier::FreeForm("slot-a".into());
        let tests = AgentIdentifier::Canonical(AgentRole::TestAgent);
        let learning = AgentIdentifier::Canonical(AgentRole::LearningAgent);
        let spec = AgentIdentifier::Canonical(AgentRole::TaskSpecifier);
        for (agent, run) in [
            (slot.clone(), "r1"),
            (tests.clone(), "r2"),
            (learning.clone(), "r3"),
            (spec.clone(), "r4"),
        ] {
            dock.note_run(
                agent,
                run,
                TerminalRunStatus::Running,
                Priority::Pipeline,
                now,
            );
        }
        assert_eq!(
            dock.tabs(),
            vec![spec.clone(), tests.clone(), learning.clone(), slot.clone()]
        );
        dock.close_tab(&tests);
        assert!(!dock.tabs().contains(&tests));
        // The same run going on keeps it closed; its next run brings it back.
        dock.note_run(
            tests.clone(),
            "r2",
            TerminalRunStatus::Running,
            Priority::Pipeline,
            now,
        );
        assert!(!dock.tabs().contains(&tests));
        dock.note_run(
            tests.clone(),
            "r5",
            TerminalRunStatus::Running,
            Priority::Pipeline,
            now,
        );
        assert!(dock.tabs().contains(&tests));
    }

    #[test]
    fn the_session_keeps_mode_height_follow_and_agent() {
        let mut dock = AgentDock {
            mode: Some(DockMode::Strip),
            height: Some(300.0),
            ..AgentDock::default()
        };
        dock.follow = FollowState::new(false);
        dock.follow.current = Some(AgentIdentifier::Canonical(AgentRole::BackendAgent));
        let session = dock.session();
        assert_eq!(session.mode.as_deref(), Some("strip"));
        assert_eq!(session.agent.as_deref(), Some("role:backend_agent"));
        let restored = AgentDock::restore(Some(&session));
        assert_eq!(restored.mode, Some(DockMode::Strip));
        assert_eq!(restored.last_visible, Some(DockMode::Strip));
        assert_eq!(restored.height, Some(300.0));
        assert!(!restored.follow.enabled);
        assert_eq!(
            restored.shown(),
            Some(AgentIdentifier::Canonical(AgentRole::BackendAgent))
        );
        assert_eq!(agent_from_key("slot:"), None);
        assert_eq!(agent_from_key("role:nobody"), None);
    }

    /// Feature 25, section 6: `pipeline:<pipelineId>/<stepId>`, and the steps
    /// after support and before free-form agents, in the canvas order.
    #[test]
    fn pipeline_steps_have_keys_and_their_place_among_the_tabs() {
        let step = |pipeline: &str, step: &str| AgentIdentifier::PipelineStep {
            pipeline_id: pipeline.into(),
            step_id: step.into(),
        };
        let review = step("p1", "review");
        assert_eq!(agent_key(&review), "pipeline:p1/review");
        assert_eq!(agent_from_key("pipeline:p1/review"), Some(review.clone()));
        assert_eq!(agent_from_key("pipeline:p1/"), None);
        assert_eq!(agent_from_key("pipeline:p1"), None);
        assert_eq!(
            AgentIdentifier::from_automation_slot("automation:project-1:p1:review"),
            Some(review.clone())
        );
        assert_eq!(
            AgentIdentifier::from_automation_slot("automation:p1:review"),
            None
        );
        assert_eq!(AgentIdentifier::from_automation_slot("free-1"), None);

        let mut dock = AgentDock::default();
        let now = Instant::now();
        let slot = AgentIdentifier::FreeForm("slot-a".into());
        let learning = AgentIdentifier::Canonical(AgentRole::LearningAgent);
        let backend = AgentIdentifier::Canonical(AgentRole::BackendAgent);
        let later = step("p2", "check");
        let first = step("p1", "summary");
        dock.set_step_order([first.clone(), review.clone(), later.clone()]);
        for (agent, run) in [
            (slot.clone(), "r1"),
            (later.clone(), "r2"),
            (learning.clone(), "r3"),
            (review.clone(), "r4"),
            (backend.clone(), "r5"),
            (first.clone(), "r6"),
        ] {
            dock.note_run(
                agent,
                run,
                TerminalRunStatus::Running,
                Priority::Automation,
                now,
            );
        }
        assert_eq!(
            dock.tabs(),
            vec![backend, learning, first, review, later, slot]
        );
    }

    #[test]
    fn stage_verifiers_round_trip_keys_and_follow_their_stage_in_tabs() {
        let verifier = AgentIdentifier::StageVerifier("ver-1".into());
        assert_eq!(agent_key(&verifier), "verifier:ver-1");
        assert_eq!(agent_from_key("verifier:ver-1"), Some(verifier.clone()));
        assert_eq!(agent_from_key("verifier:"), None);
        assert_eq!(
            AgentIdentifier::from_automation_slot("verifier:proj-1:ver-1"),
            Some(verifier.clone())
        );

        let mut dock = AgentDock::default();
        let now = Instant::now();
        let verifier_0 = AgentIdentifier::StageVerifier("ver-code-0".into());
        let verifier_1 = AgentIdentifier::StageVerifier("ver-code-1".into());
        let step = AgentIdentifier::PipelineStep {
            pipeline_id: "p1".into(),
            step_id: "s1".into(),
        };
        let slot = AgentIdentifier::FreeForm("free-1".into());
        let intake = AgentIdentifier::Canonical(AgentRole::IntakeRouter);
        let code = AgentIdentifier::Canonical(AgentRole::BackendAgent);
        let test_agent = AgentIdentifier::Canonical(AgentRole::TestAgent);
        let learning = AgentIdentifier::Canonical(AgentRole::LearningAgent);
        dock.set_verifier_order([(verifier_0.clone(), 3, 0), (verifier_1.clone(), 3, 1)]);
        dock.set_step_order([step.clone()]);
        for (agent, run) in [
            (slot.clone(), "r1"),
            (step.clone(), "r2"),
            (learning.clone(), "r3"),
            (test_agent.clone(), "r4"),
            (verifier_1.clone(), "r5"),
            (code.clone(), "r6"),
            (verifier_0.clone(), "r7"),
            (intake.clone(), "r8"),
        ] {
            dock.note_run(
                agent,
                run,
                TerminalRunStatus::Running,
                Priority::Pipeline,
                now,
            );
        }
        assert_eq!(
            dock.tabs(),
            vec![
                intake, code, verifier_0, verifier_1, test_agent, learning, step, slot,
            ]
        );
    }
}
