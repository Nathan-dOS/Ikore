//! Which agent the bottom dock shows (ADR-040, M4; specification 10): it
//! follows the agent that started most recently, with pipeline stages ahead
//! of interactive runs and those ahead of background work, waits a moment
//! before it switches, and never switches while the developer is reading.
//! Pure: no GPUI, and every input carries its instant.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::agents::AgentIdentifier;

/// How long a candidate must stay the best while the shown agent runs.
pub(crate) const DEBOUNCE_RUNNING: Duration = Duration::from_millis(1_500);
/// How long a candidate must stay the best once the shown agent stopped.
pub(crate) const DEBOUNCE_IDLE: Duration = Duration::from_millis(300);
/// No switch this long after the developer touched the dock.
pub(crate) const INTERACTION_GUARD: Duration = Duration::from_secs(5);
/// How often a pending switch is checked again.
pub(crate) const TICK: Duration = Duration::from_millis(250);

/// Which runs may take the dock from which (rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Priority {
    Background,
    /// A step of the developer's own pipelines (feature 25, E7): it never
    /// takes the dock from the Ikore pipeline or from a run the developer
    /// started, and shows when they are idle.
    Automation,
    Interactive,
    Pipeline,
}

impl Priority {
    /// A run's priority from where it came from.
    pub(crate) fn of(origin: ikore_core::terminals::ExecutionOrigin) -> Self {
        use ikore_core::terminals::ExecutionOrigin;
        match origin {
            ExecutionOrigin::Pipeline
            | ExecutionOrigin::Verification
            | ExecutionOrigin::Feature
            | ExecutionOrigin::Review => Self::Pipeline,
            ExecutionOrigin::Manual => Self::Interactive,
            ExecutionOrigin::Automation => Self::Automation,
            ExecutionOrigin::Learning
            | ExecutionOrigin::Notes
            | ExecutionOrigin::Reconciliation => Self::Background,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FollowMode {
    Following,
    /// The developer chose this agent; nothing switches until they resume.
    Pinned(AgentIdentifier),
}

/// What the dock learned (specification 10.2).
#[derive(Debug, Clone)]
pub(crate) enum FollowInput {
    /// A run, or an API route, of `agent` started.
    RunActivated {
        agent: AgentIdentifier,
        run_id: String,
        priority: Priority,
        at: Instant,
    },
    RunEnded {
        agent: AgentIdentifier,
        run_id: String,
        at: Instant,
    },
    /// Pointer, wheel or keys in the dock.
    Interaction {
        at: Instant,
    },
    ScrolledUp {
        up: bool,
        at: Instant,
    },
    SearchFocused {
        focused: bool,
        at: Instant,
    },
    Pin(AgentIdentifier),
    ResumeFollowing,
    SetEnabled {
        enabled: bool,
        at: Instant,
    },
    Tick(Instant),
}

#[derive(Debug, Clone)]
struct Running {
    run_id: String,
    priority: Priority,
    started: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct FollowState {
    /// The project's preference; on by default.
    pub(crate) enabled: bool,
    pub(crate) mode: FollowMode,
    pub(crate) current: Option<AgentIdentifier>,
    /// The candidate and since when it has been the best.
    pub(crate) pending: Option<(AgentIdentifier, Instant)>,
    pub(crate) guard_until: Option<Instant>,
    pub(crate) scrolled_up: bool,
    pub(crate) search_focused: bool,
    running: HashMap<AgentIdentifier, Running>,
    /// Agents whose tab carries a dot: a run started that the dock did not show.
    marked: HashSet<AgentIdentifier>,
}

impl Default for FollowState {
    fn default() -> Self {
        Self::new(true)
    }
}

impl FollowState {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            mode: FollowMode::Following,
            current: None,
            pending: None,
            guard_until: None,
            scrolled_up: false,
            search_focused: false,
            running: HashMap::new(),
            marked: HashSet::new(),
        }
    }

    pub(crate) fn is_marked(&self, agent: &AgentIdentifier) -> bool {
        self.marked.contains(agent)
    }

    pub(crate) fn pinned(&self) -> bool {
        matches!(self.mode, FollowMode::Pinned(_))
    }

    /// The agent a `Switch to {agent} →` chip offers: a switch the developer's
    /// reading holds back (rule 4).
    pub(crate) fn chip(&self, now: Instant) -> Option<&AgentIdentifier> {
        self.pending
            .as_ref()
            .filter(|_| self.guarded(now))
            .map(|(agent, _)| agent)
    }

    /// Whether a tick should come: a switch waits for its time.
    pub(crate) fn needs_tick(&self) -> bool {
        self.pending.is_some()
    }

    /// Applies one input; returns whether the shown agent changed.
    pub(crate) fn apply(&mut self, input: FollowInput) -> bool {
        let now = match input {
            FollowInput::RunActivated {
                agent,
                run_id,
                priority,
                at,
            } => {
                if self.current.as_ref() != Some(&agent) {
                    self.marked.insert(agent.clone());
                }
                self.running.insert(
                    agent,
                    Running {
                        run_id,
                        priority,
                        started: at,
                    },
                );
                at
            }
            FollowInput::RunEnded { agent, run_id, at } => {
                if self
                    .running
                    .get(&agent)
                    .is_some_and(|running| running.run_id == run_id)
                {
                    self.running.remove(&agent);
                }
                at
            }
            FollowInput::Interaction { at } => {
                self.guard_until = Some(at + INTERACTION_GUARD);
                at
            }
            FollowInput::ScrolledUp { up, at } => {
                self.scrolled_up = up;
                at
            }
            FollowInput::SearchFocused { focused, at } => {
                self.search_focused = focused;
                at
            }
            FollowInput::Pin(agent) => {
                self.marked.remove(&agent);
                self.pending = None;
                let changed = self.current.as_ref() != Some(&agent);
                self.current = Some(agent.clone());
                self.mode = FollowMode::Pinned(agent);
                return changed;
            }
            FollowInput::ResumeFollowing => {
                self.mode = FollowMode::Following;
                self.pending = None;
                // Straight to the best candidate, without the debounce.
                return self
                    .best_running()
                    .is_some_and(|agent| self.switch_to(agent));
            }
            FollowInput::SetEnabled { enabled, at } => {
                self.enabled = enabled;
                at
            }
            FollowInput::Tick(at) => at,
        };
        self.evaluate(now)
    }

    fn guarded(&self, now: Instant) -> bool {
        self.scrolled_up || self.search_focused || self.guard_until.is_some_and(|until| now < until)
    }

    /// The best running agent the dock may switch to (rule 2): any, when the
    /// shown agent stopped; a higher priority, while it runs. Ties go to the
    /// one that started last.
    fn best(&self) -> Option<AgentIdentifier> {
        let current = self
            .current
            .as_ref()
            .and_then(|current| self.running.get(current));
        self.running
            .iter()
            .filter(|(agent, _)| self.current.as_ref() != Some(*agent))
            .filter(|(_, running)| {
                current.is_none_or(|current| running.priority > current.priority)
            })
            .max_by_key(|(_, running)| (running.priority, running.started))
            .map(|(agent, _)| agent.clone())
    }

    /// The running agent with the highest priority, the last started first;
    /// it may be the one shown.
    fn best_running(&self) -> Option<AgentIdentifier> {
        self.running
            .iter()
            .max_by_key(|(_, running)| (running.priority, running.started))
            .map(|(agent, _)| agent.clone())
    }

    fn switch_to(&mut self, agent: AgentIdentifier) -> bool {
        self.marked.remove(&agent);
        self.pending = None;
        let changed = self.current.as_ref() != Some(&agent);
        self.current = Some(agent);
        changed
    }

    fn evaluate(&mut self, now: Instant) -> bool {
        if !self.enabled || self.pinned() {
            self.pending = None;
            return false;
        }
        let Some(candidate) = self.best() else {
            self.pending = None;
            return false;
        };
        // The timer runs while the same candidate stays the best.
        let since = match &self.pending {
            Some((agent, since)) if *agent == candidate => *since,
            _ => now,
        };
        self.pending = Some((candidate.clone(), since));
        let current_running = self
            .current
            .as_ref()
            .is_some_and(|current| self.running.contains_key(current));
        let debounce = if current_running {
            DEBOUNCE_RUNNING
        } else {
            DEBOUNCE_IDLE
        };
        if now.saturating_duration_since(since) >= debounce && !self.guarded(now) {
            return self.switch_to(candidate);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::llm::AgentRole;

    fn agent(role: AgentRole) -> AgentIdentifier {
        AgentIdentifier::Canonical(role)
    }

    fn free(slot: &str) -> AgentIdentifier {
        AgentIdentifier::FreeForm(slot.into())
    }

    struct Clock(Instant);

    impl Clock {
        fn at(&self, millis: u64) -> Instant {
            self.0 + Duration::from_millis(millis)
        }
    }

    fn start(
        state: &mut FollowState,
        who: &AgentIdentifier,
        run: &str,
        priority: Priority,
        at: Instant,
    ) -> bool {
        state.apply(FollowInput::RunActivated {
            agent: who.clone(),
            run_id: run.into(),
            priority,
            at,
        })
    }

    fn end(state: &mut FollowState, who: &AgentIdentifier, run: &str, at: Instant) -> bool {
        state.apply(FollowInput::RunEnded {
            agent: who.clone(),
            run_id: run.into(),
            at,
        })
    }

    /// Ticks every 250 ms from `from` to `to`, inclusive.
    fn tick(state: &mut FollowState, clock: &Clock, from: u64, to: u64) {
        let mut at = from;
        while at <= to {
            state.apply(FollowInput::Tick(clock.at(at)));
            at += 250;
        }
    }

    /// The vectors of specification 10.4.
    #[test]
    fn follow_vectors() {
        let backend = agent(AgentRole::BackendAgent);
        let tests = agent(AgentRole::TestAgent);
        let learning = free("learning");
        let spec = agent(AgentRole::TaskSpecifier);

        // 1. A background run never takes the dock from a running stage.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        tick(&mut state, &clock, 0, 500);
        assert_eq!(state.current.as_ref(), Some(&backend));
        start(
            &mut state,
            &learning,
            "l1",
            Priority::Background,
            clock.at(1_000),
        );
        tick(&mut state, &clock, 1_000, 5_000);
        assert_eq!(state.current.as_ref(), Some(&backend), "vector 1");
        assert!(state.is_marked(&learning));

        // 2. Once the stage ended, the next run takes the dock after 0.3 s.
        end(&mut state, &backend, "b1", clock.at(6_000));
        end(&mut state, &learning, "l1", clock.at(6_000));
        start(
            &mut state,
            &learning,
            "l2",
            Priority::Background,
            clock.at(8_000),
        );
        tick(&mut state, &clock, 8_000, 8_250);
        assert_eq!(state.current.as_ref(), Some(&backend));
        tick(&mut state, &clock, 8_300, 8_300);
        assert_eq!(state.current.as_ref(), Some(&learning), "vector 2");
        assert!(!state.is_marked(&learning));

        // 3. Same priority while the shown stage runs: it stays; when it
        //    ends, the other takes the dock after 0.3 s.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        tick(&mut state, &clock, 0, 500);
        start(
            &mut state,
            &tests,
            "t1",
            Priority::Pipeline,
            clock.at(1_000),
        );
        tick(&mut state, &clock, 1_000, 4_000);
        assert_eq!(state.current.as_ref(), Some(&backend), "vector 3");
        end(&mut state, &backend, "b1", clock.at(4_100));
        tick(&mut state, &clock, 4_100, 4_350);
        assert_eq!(state.current.as_ref(), Some(&backend));
        tick(&mut state, &clock, 4_400, 4_400);
        assert_eq!(
            state.current.as_ref(),
            Some(&tests),
            "vector 3, after the end"
        );

        // 4. One ends and the other starts in the same second.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        tick(&mut state, &clock, 0, 500);
        end(&mut state, &backend, "b1", clock.at(1_000));
        start(
            &mut state,
            &tests,
            "t1",
            Priority::Pipeline,
            clock.at(1_200),
        );
        tick(&mut state, &clock, 1_250, 1_500);
        assert_eq!(state.current.as_ref(), Some(&tests), "vector 4");

        // 5. A stage takes the dock from background work after 1.5 s.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(
            &mut state,
            &learning,
            "l1",
            Priority::Background,
            clock.at(0),
        );
        tick(&mut state, &clock, 0, 500);
        assert_eq!(state.current.as_ref(), Some(&learning));
        start(
            &mut state,
            &backend,
            "b1",
            Priority::Pipeline,
            clock.at(1_000),
        );
        tick(&mut state, &clock, 1_000, 2_250);
        assert_eq!(state.current.as_ref(), Some(&learning));
        tick(&mut state, &clock, 2_500, 2_500);
        assert_eq!(state.current.as_ref(), Some(&backend), "vector 5");

        // 6. Vector 5 while the developer scrolls: the chip shows, and the
        //    switch comes 5 s after the last touch.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(
            &mut state,
            &learning,
            "l1",
            Priority::Background,
            clock.at(0),
        );
        tick(&mut state, &clock, 0, 500);
        start(
            &mut state,
            &backend,
            "b1",
            Priority::Pipeline,
            clock.at(1_000),
        );
        for at in [1_200, 2_000, 3_000] {
            state.apply(FollowInput::Interaction { at: clock.at(at) });
        }
        tick(&mut state, &clock, 1_000, 7_750);
        assert_eq!(state.current.as_ref(), Some(&learning));
        assert_eq!(
            state.chip(clock.at(7_750)),
            Some(&backend),
            "vector 6, chip"
        );
        tick(&mut state, &clock, 8_000, 8_000);
        assert_eq!(state.current.as_ref(), Some(&backend), "vector 6");
        assert_eq!(state.chip(clock.at(8_000)), None);

        // 7. A tab the developer clicks stays until they resume.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        tick(&mut state, &clock, 0, 500);
        state.apply(FollowInput::Pin(spec.clone()));
        start(
            &mut state,
            &tests,
            "t1",
            Priority::Pipeline,
            clock.at(1_000),
        );
        end(&mut state, &backend, "b1", clock.at(1_100));
        tick(&mut state, &clock, 1_000, 6_000);
        assert_eq!(state.current.as_ref(), Some(&spec), "vector 7");
        assert!(state.apply(FollowInput::ResumeFollowing));
        assert_eq!(state.current.as_ref(), Some(&tests));

        // 8. Off: nothing switches, every new run gets its dot.
        let clock = Clock(Instant::now());
        let mut state = FollowState::new(false);
        for (who, run) in [(&backend, "b1"), (&tests, "t1"), (&learning, "l1")] {
            start(&mut state, who, run, Priority::Pipeline, clock.at(0));
        }
        tick(&mut state, &clock, 0, 5_000);
        assert_eq!(state.current, None, "vector 8");
        assert!(
            [&backend, &tests, &learning]
                .iter()
                .all(|who| state.is_marked(who))
        );

        // 9. A run of 0.8 s inside another's debounce never takes the dock.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(
            &mut state,
            &learning,
            "l1",
            Priority::Background,
            clock.at(0),
        );
        tick(&mut state, &clock, 0, 500);
        start(
            &mut state,
            &backend,
            "b1",
            Priority::Pipeline,
            clock.at(1_000),
        );
        start(
            &mut state,
            &tests,
            "t1",
            Priority::Pipeline,
            clock.at(1_200),
        );
        tick(&mut state, &clock, 1_000, 1_750);
        end(&mut state, &tests, "t1", clock.at(2_000));
        tick(&mut state, &clock, 2_000, 3_250);
        assert_eq!(state.current.as_ref(), Some(&learning));
        tick(&mut state, &clock, 3_500, 3_500);
        assert_eq!(state.current.as_ref(), Some(&backend), "vector 9");

        // 10. An API route follows the same rules as a run.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        let contract = agent(AgentRole::ContractAgent);
        start(
            &mut state,
            &contract,
            "route-1",
            Priority::Pipeline,
            clock.at(0),
        );
        tick(&mut state, &clock, 0, 500);
        assert_eq!(state.current.as_ref(), Some(&contract), "vector 10");
    }

    #[test]
    fn pinned_survives_new_runs_until_resume() {
        let clock = Clock(Instant::now());
        let backend = agent(AgentRole::BackendAgent);
        let tests = agent(AgentRole::TestAgent);
        let mut state = FollowState::default();
        state.apply(FollowInput::Pin(backend.clone()));
        for (index, run) in ["t1", "t2", "t3"].into_iter().enumerate() {
            let at = 1_000 * (index as u64 + 1);
            start(&mut state, &tests, run, Priority::Pipeline, clock.at(at));
            tick(&mut state, &clock, at, at + 2_000);
            end(&mut state, &tests, run, clock.at(at + 2_100));
        }
        assert_eq!(state.current.as_ref(), Some(&backend));
        assert!(state.is_marked(&tests));
        state.apply(FollowInput::ResumeFollowing);
        assert_eq!(state.mode, FollowMode::Following);
    }

    #[test]
    fn disabled_never_switches() {
        let clock = Clock(Instant::now());
        let backend = agent(AgentRole::BackendAgent);
        let mut state = FollowState::default();
        state.apply(FollowInput::SetEnabled {
            enabled: false,
            at: clock.at(0),
        });
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        tick(&mut state, &clock, 0, 10_000);
        assert_eq!(state.current, None);
        // On again, the running stage takes the dock by the same rules.
        state.apply(FollowInput::SetEnabled {
            enabled: true,
            at: clock.at(10_000),
        });
        tick(&mut state, &clock, 10_000, 10_500);
        assert_eq!(state.current.as_ref(), Some(&backend));
    }

    /// Feature 25, E7: a step of the developer's own pipelines sits between
    /// interactive runs and background work.
    #[test]
    fn automation_vectors() {
        let backend = agent(AgentRole::BackendAgent);
        let manual = agent(AgentRole::TaskSpecifier);
        let learning = free("learning");
        let step = AgentIdentifier::PipelineStep {
            pipeline_id: "p1".into(),
            step_id: "review".into(),
        };
        assert!(Priority::Background < Priority::Automation);
        assert!(Priority::Automation < Priority::Interactive);
        assert_eq!(
            Priority::of(ikore_core::terminals::ExecutionOrigin::Automation),
            Priority::Automation
        );
        assert_eq!(
            Priority::of(ikore_core::terminals::ExecutionOrigin::Verification),
            Priority::Pipeline
        );

        // A step never takes the dock from a running stage.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        tick(&mut state, &clock, 0, 500);
        start(
            &mut state,
            &step,
            "s1",
            Priority::Automation,
            clock.at(1_000),
        );
        tick(&mut state, &clock, 1_000, 5_000);
        assert_eq!(state.current.as_ref(), Some(&backend), "the stage stays");
        assert!(state.is_marked(&step));
        // Once the stage ended, the step shows after 0.3 s.
        end(&mut state, &backend, "b1", clock.at(6_000));
        tick(&mut state, &clock, 6_000, 6_250);
        assert_eq!(state.current.as_ref(), Some(&backend));
        tick(&mut state, &clock, 6_300, 6_300);
        assert_eq!(
            state.current.as_ref(),
            Some(&step),
            "the idle default gives way"
        );

        // A step takes the dock from background work after 1.5 s.
        let clock = Clock(Instant::now());
        let mut state = FollowState::default();
        start(
            &mut state,
            &learning,
            "l1",
            Priority::Background,
            clock.at(0),
        );
        tick(&mut state, &clock, 0, 500);
        start(
            &mut state,
            &step,
            "s1",
            Priority::Automation,
            clock.at(1_000),
        );
        tick(&mut state, &clock, 1_000, 2_250);
        assert_eq!(state.current.as_ref(), Some(&learning));
        tick(&mut state, &clock, 2_500, 2_500);
        assert_eq!(state.current.as_ref(), Some(&step), "over background work");

        // A run the developer started takes the dock from a step, and a step
        // never takes it from such a run.
        start(
            &mut state,
            &manual,
            "m1",
            Priority::Interactive,
            clock.at(3_000),
        );
        tick(&mut state, &clock, 3_000, 4_500);
        assert_eq!(
            state.current.as_ref(),
            Some(&manual),
            "an interactive run wins"
        );
        start(
            &mut state,
            &step,
            "s2",
            Priority::Automation,
            clock.at(5_000),
        );
        tick(&mut state, &clock, 5_000, 9_000);
        assert_eq!(
            state.current.as_ref(),
            Some(&manual),
            "the developer's run stays"
        );
    }

    #[test]
    fn host_jobs_without_agent_do_not_switch() {
        // A job with no agent never reaches the state as a run: only runs and
        // API routes do. A run that ends without having started changes
        // nothing either.
        let clock = Clock(Instant::now());
        let backend = agent(AgentRole::BackendAgent);
        let mut state = FollowState::default();
        assert!(!end(&mut state, &backend, "b0", clock.at(0)));
        tick(&mut state, &clock, 0, 2_000);
        assert_eq!(state.current, None);
        assert!(!state.needs_tick());
    }

    #[test]
    fn resume_goes_to_the_best_running_agent_which_may_be_the_shown_one() {
        let clock = Clock(Instant::now());
        let backend = agent(AgentRole::BackendAgent);
        let spec = agent(AgentRole::TaskSpecifier);
        let mut state = FollowState::default();
        start(&mut state, &backend, "b1", Priority::Pipeline, clock.at(0));
        start(
            &mut state,
            &spec,
            "s1",
            Priority::Interactive,
            clock.at(100),
        );
        state.apply(FollowInput::Pin(backend.clone()));
        // The stage outranks the manual run that started later: it stays.
        assert!(!state.apply(FollowInput::ResumeFollowing));
        assert_eq!(state.current.as_ref(), Some(&backend));
        assert!(state.is_marked(&spec));
        // Once the stage ended, resuming goes to the manual run at once.
        end(&mut state, &backend, "b1", clock.at(200));
        state.apply(FollowInput::Pin(backend.clone()));
        assert!(state.apply(FollowInput::ResumeFollowing));
        assert_eq!(state.current.as_ref(), Some(&spec));
    }
}
