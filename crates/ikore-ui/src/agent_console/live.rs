//! What a run does now, from its activity pulses (ADR-040, M5 and M6;
//! specification 5.6): the phase and since when, the last sign of life, the
//! counters, reasoning included, and the actions the run started, shown
//! provisionally until its persisted events arrive. Pure: no GPUI.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ikore_core::terminals::{
    ActivityCounters, ActivityPhase, AgentActivityPulseV1, AgentActivitySnapshotV1,
    PulseActionDelta, PulseTarget,
};

use super::projection::{ActionBlock, ActionState, BlockBody, ConsoleBlock};

/// Provisional actions one run keeps.
const MAX_PROVISIONAL: usize = 200;
/// A run quiet this long says so in its "now" line.
const QUIET_AFTER: Duration = Duration::from_secs(10);
/// Ended runs whose last counters stay.
const MAX_FINISHED: usize = 64;

/// One live run's activity.
#[derive(Debug)]
pub(crate) struct LiveRunActivity {
    pub(crate) phase: ActivityPhase,
    pub(crate) phase_since: Instant,
    pub(crate) last_activity_at: Instant,
    pub(crate) counters: ActivityCounters,
    /// The counters' elapsed time, when the pulse that carried it arrived.
    received_at: Instant,
    last_sequence: u64,
    /// The actions the pulses announced, oldest first; one allocation each,
    /// so a console sees by identity which changed.
    provisional: Vec<Rc<ConsoleBlock>>,
    /// Bumped whenever the provisional actions change.
    pub(crate) revision: u64,
}

fn target_text(target: &PulseTarget) -> Option<String> {
    match target {
        PulseTarget::WorkspacePath(path) => Some(path.clone()),
        PulseTarget::Program(program) => Some(program.clone()),
        PulseTarget::Host(host) => Some(host.clone()),
        PulseTarget::Withheld => Some("withheld".to_owned()),
    }
}

impl LiveRunActivity {
    fn new(pulse: &AgentActivityPulseV1, now: Instant) -> Self {
        Self {
            phase: pulse.phase,
            phase_since: now,
            last_activity_at: now,
            counters: ActivityCounters::default(),
            received_at: now,
            last_sequence: 0,
            provisional: Vec::new(),
            revision: 0,
        }
    }

    fn apply(&mut self, pulse: &AgentActivityPulseV1, now: Instant) {
        let ago = |ms: u64| now.checked_sub(Duration::from_millis(ms)).unwrap_or(now);
        self.last_sequence = pulse.pulse_sequence;
        self.phase = pulse.phase;
        self.phase_since = ago(pulse.phase_since_ms);
        self.last_activity_at = ago(pulse.last_activity_ms_ago);
        self.counters = pulse.counters.clone();
        self.received_at = now;
        if pulse.actions.is_empty() {
            return;
        }
        for delta in &pulse.actions {
            self.apply_delta(delta);
        }
        if self.provisional.len() > MAX_PROVISIONAL {
            let excess = self.provisional.len() - MAX_PROVISIONAL;
            self.provisional.drain(..excess);
        }
        self.revision += 1;
    }

    fn apply_delta(&mut self, delta: &PulseActionDelta) {
        match delta {
            PulseActionDelta::Started {
                ordinal,
                kind,
                name,
                target,
            } => {
                if self.position(*ordinal).is_some() {
                    return;
                }
                self.provisional.push(Rc::new(ConsoleBlock {
                    key: format!("live:{ordinal}"),
                    body: BlockBody::Action(ActionBlock {
                        ordinal: Some(*ordinal),
                        kind: *kind,
                        name: name.clone(),
                        target: target.as_ref().and_then(target_text),
                        state: ActionState::Running,
                        result: None,
                    }),
                }));
            }
            PulseActionDelta::Finished {
                ordinal,
                status,
                duration_ms,
            } => {
                let Some(index) = self.position(*ordinal) else {
                    return;
                };
                if let BlockBody::Action(action) =
                    &mut Rc::make_mut(&mut self.provisional[index]).body
                {
                    action.state = ActionState::Done {
                        status: *status,
                        exit_code: None,
                        duration_ms: Some(*duration_ms),
                    };
                }
            }
        }
    }

    fn position(&self, ordinal: u32) -> Option<usize> {
        self.provisional.iter().position(|block| {
            matches!(&block.body, BlockBody::Action(action) if action.ordinal == Some(ordinal))
        })
    }

    pub(crate) fn provisional(&self) -> &[Rc<ConsoleBlock>] {
        &self.provisional
    }

    /// The action still running that started last.
    fn current_action(&self) -> Option<&ActionBlock> {
        self.provisional
            .iter()
            .rev()
            .find_map(|block| match &block.body {
                BlockBody::Action(action) if action.state == ActionState::Running => Some(action),
                _ => None,
            })
    }

    /// `Running tool · Read src/main.rs · 4 read · 2 commands · 1:12 · quiet 14 s`.
    pub(crate) fn line(&self, now: Instant) -> String {
        let mut parts = vec![self.phase.label().to_owned()];
        if self.phase == ActivityPhase::RunningTool
            && let Some(action) = self.current_action()
        {
            parts.push(match &action.target {
                Some(target) => format!("{} {target}", action.name),
                None => action.name.clone(),
            });
        }
        let counters = &self.counters;
        for (count, label) in [
            (counters.files_read, "read"),
            (counters.files_changed, "changed"),
            (counters.searches, "searches"),
            (counters.commands, "commands"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {label}"));
            }
        }
        // ADR-040 M6: how much the model reasoned, never what.
        if counters.reasoning_blocks > 0 || counters.reasoning_tokens.is_some() {
            parts.push(reasoning_label(
                counters.reasoning_blocks,
                counters.reasoning_tokens,
            ));
        }
        let elapsed = Duration::from_millis(counters.elapsed_ms)
            + now.saturating_duration_since(self.received_at);
        parts.push(super::view::duration_label(
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        ));
        let quiet = now.saturating_duration_since(self.last_activity_at);
        if quiet >= QUIET_AFTER && self.phase != ActivityPhase::Finishing {
            parts.push(format!("quiet {} s", quiet.as_secs()));
        }
        parts.join(" · ")
    }
}

/// `2 thinking · 1.4k reasoning tokens` (M6: phase and counts, no text).
pub(crate) fn reasoning_label(blocks: u32, tokens: Option<u64>) -> String {
    let mut label = match blocks {
        0 => String::new(),
        1 => "1 thinking".to_owned(),
        count => format!("{count} thinking"),
    };
    if let Some(tokens) = tokens {
        if !label.is_empty() {
            label.push_str(" · ");
        }
        label.push_str(&format!("{} reasoning tokens", compact(tokens)));
    }
    label
}

fn compact(count: u64) -> String {
    if count >= 10_000 {
        format!("{}k", count / 1000)
    } else if count >= 1000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else {
        count.to_string()
    }
}

/// The live runs the UI knows of.
#[derive(Debug, Default)]
pub(crate) struct LiveActivity {
    runs: HashMap<String, LiveRunActivity>,
    /// The last counters of runs that ended in this session, for their header (M6).
    finished: HashMap<String, ActivityCounters>,
    finished_order: VecDeque<String>,
}

impl LiveActivity {
    pub(crate) fn get(&self, run_id: &str) -> Option<&LiveRunActivity> {
        self.runs.get(run_id)
    }

    /// Applies a pulse, unless an equal or newer one of the run came first.
    /// Returns whether it changed anything.
    pub(crate) fn record(&mut self, pulse: &AgentActivityPulseV1, now: Instant) -> bool {
        let run = self
            .runs
            .entry(pulse.run_id.clone())
            .or_insert_with(|| LiveRunActivity::new(pulse, now));
        if run.last_sequence >= pulse.pulse_sequence {
            return false;
        }
        run.apply(pulse, now);
        true
    }

    /// A snapshot's live runs, for a UI that opened mid-run.
    pub(crate) fn seed(&mut self, snapshots: &[AgentActivitySnapshotV1], now: Instant) {
        for snapshot in snapshots {
            let mut pulse = snapshot.latest.clone();
            if self
                .runs
                .get(&pulse.run_id)
                .is_some_and(|run| run.last_sequence >= pulse.pulse_sequence)
            {
                continue;
            }
            pulse.actions = snapshot.recent_actions.clone();
            self.runs.remove(&pulse.run_id);
            self.record(&pulse, now);
        }
    }

    /// The run's persisted actions arrived: they take the provisional ones'
    /// place (specification 5.6, reconciliation).
    pub(crate) fn clear_provisional(&mut self, run_id: &str) {
        if let Some(run) = self.runs.get_mut(run_id)
            && !run.provisional.is_empty()
        {
            run.provisional.clear();
            run.revision += 1;
        }
    }

    /// The run ended: nothing about it is live any more. Its last counters
    /// stay for the run's header, how much the model reasoned included (M6).
    pub(crate) fn finish(&mut self, run_id: &str) {
        let Some(run) = self.runs.remove(run_id) else {
            return;
        };
        if self
            .finished
            .insert(run_id.to_owned(), run.counters)
            .is_none()
        {
            self.finished_order.push_back(run_id.to_owned());
        }
        while self.finished_order.len() > MAX_FINISHED {
            if let Some(oldest) = self.finished_order.pop_front() {
                self.finished.remove(&oldest);
            }
        }
    }

    /// The last counters of a run that ended in this session.
    pub(crate) fn finished(&self, run_id: &str) -> Option<&ActivityCounters> {
        self.finished.get(run_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::terminals::{ActionStatus, ToolKind};

    fn pulse(
        sequence: u64,
        phase: ActivityPhase,
        actions: Vec<PulseActionDelta>,
    ) -> AgentActivityPulseV1 {
        AgentActivityPulseV1 {
            protocol_version: 1,
            project_id: "p".into(),
            run_id: "run-1".into(),
            agent_role: None,
            slot_id: None,
            pulse_sequence: sequence,
            emitted_at: "2026-11-15T10:00:00.000Z".into(),
            phase,
            phase_since_ms: 0,
            last_activity_ms_ago: 0,
            actions,
            actions_dropped: 0,
            counters: ActivityCounters {
                tool_calls: 1,
                files_read: 1,
                elapsed_ms: 72_000,
                ..ActivityCounters::default()
            },
        }
    }

    fn started(ordinal: u32) -> PulseActionDelta {
        PulseActionDelta::Started {
            ordinal,
            kind: ToolKind::Read,
            name: "Read".into(),
            target: Some(PulseTarget::WorkspacePath("src/main.rs".into())),
        }
    }

    /// Specification 13: applying a pulse costs at most 0.2 ms on average,
    /// without rendering, the "now" line included. A measurement for the
    /// validation record, run on demand:
    /// `cargo test -p ikore-ui --lib a_pulse_costs -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run on demand for the validation record"]
    fn a_pulse_costs_under_two_tenths_of_a_millisecond() {
        let now = Instant::now();
        let mut live = LiveActivity::default();
        // Ten minutes at four pulses a second, each closing an action and starting the next.
        let pulses: Vec<_> = (1..=2_400_u32)
            .map(|ordinal| {
                pulse(
                    u64::from(ordinal),
                    ActivityPhase::RunningTool,
                    vec![
                        PulseActionDelta::Finished {
                            ordinal: ordinal.saturating_sub(1).max(1),
                            status: ActionStatus::Ok,
                            duration_ms: 10,
                        },
                        started(ordinal),
                    ],
                )
            })
            .collect();
        let measured = Instant::now();
        for pulse in &pulses {
            live.record(pulse, now);
            let line = live.get("run-1").map(|run| run.line(now));
            assert!(line.is_some());
        }
        let mean = measured.elapsed() / 2_400;
        eprintln!("pulse: {mean:?} on average over 2400 pulses");
        assert!(mean.as_micros() < 200, "{mean:?} per pulse");
    }

    #[test]
    fn pulses_apply_in_order_and_an_older_one_is_dropped() {
        let now = Instant::now();
        let mut live = LiveActivity::default();
        assert!(live.record(&pulse(2, ActivityPhase::RunningTool, vec![started(1)]), now));
        assert!(!live.record(&pulse(1, ActivityPhase::Thinking, Vec::new()), now));
        let run = live.get("run-1").unwrap();
        assert_eq!(run.phase, ActivityPhase::RunningTool);
        assert_eq!(run.provisional().len(), 1);
        assert_eq!(
            run.line(now),
            "Running tool · Read src/main.rs · 1 read · 1:12"
        );
        // The result closes the same block, as a new allocation.
        let before = Rc::clone(&run.provisional()[0]);
        live.record(
            &pulse(
                3,
                ActivityPhase::Thinking,
                vec![PulseActionDelta::Finished {
                    ordinal: 1,
                    status: ActionStatus::Ok,
                    duration_ms: 230,
                }],
            ),
            now,
        );
        let run = live.get("run-1").unwrap();
        assert!(!Rc::ptr_eq(&before, &run.provisional()[0]));
        assert!(matches!(
            &run.provisional()[0].body,
            BlockBody::Action(action) if matches!(action.state, ActionState::Done { .. })
        ));
    }

    #[test]
    fn persisted_actions_replace_the_provisional_ones_and_the_end_clears_the_run() {
        let now = Instant::now();
        let mut live = LiveActivity::default();
        live.record(
            &pulse(1, ActivityPhase::RunningTool, vec![started(1), started(2)]),
            now,
        );
        live.clear_provisional("run-1");
        assert!(live.get("run-1").unwrap().provisional().is_empty());
        live.finish("run-1");
        assert!(live.get("run-1").is_none());
        // M6: the last counters stay for the run's header.
        assert!(live.finished("run-1").is_some());
    }

    #[test]
    fn reasoning_shows_as_counts_only() {
        assert_eq!(
            reasoning_label(2, Some(1_430)),
            "2 thinking · 1.4k reasoning tokens"
        );
        assert_eq!(reasoning_label(0, Some(80)), "80 reasoning tokens");
        assert_eq!(reasoning_label(1, None), "1 thinking");
        let now = Instant::now();
        let mut live = LiveActivity::default();
        let mut thinking = pulse(1, ActivityPhase::Thinking, Vec::new());
        thinking.counters.reasoning_blocks = 1;
        thinking.counters.reasoning_tokens = Some(12_000);
        thinking.last_activity_ms_ago = 15_000;
        live.record(&thinking, now);
        assert_eq!(
            live.get("run-1").unwrap().line(now),
            "Thinking · 1 read · 1 thinking · 12k reasoning tokens · 1:12 · quiet 15 s"
        );
    }

    #[test]
    fn a_snapshot_seeds_a_run_the_ui_did_not_see_start() {
        let now = Instant::now();
        let mut live = LiveActivity::default();
        live.seed(
            &[AgentActivitySnapshotV1 {
                latest: pulse(7, ActivityPhase::Writing, Vec::new()),
                recent_actions: vec![started(1)],
            }],
            now,
        );
        let run = live.get("run-1").unwrap();
        assert_eq!(run.phase, ActivityPhase::Writing);
        assert_eq!(run.provisional().len(), 1);
    }
}
