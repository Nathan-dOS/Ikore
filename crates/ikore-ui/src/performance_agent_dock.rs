//! ADR-040 M8: the UI latency probe's agent dock workload. Three CLI runs
//! stream at once, as a composed stage does, and the agent dock is expanded
//! on the one it follows. In memory only: no CLI, repository or credential
//! is touched, like the Companion workload beside it.

use std::time::Instant;

use ikore_core::llm::AgentRole;
use ikore_core::terminals::{
    ActivityCounters, ActivityPhase, AgentActivityPulseV1, AgentExecutionMode,
    AgentTerminalSnapshot, CliAgentConfig, CliBackend, CliModelSelectionSource, EventDetailV1,
    ExecutionOrigin, FreeFormTerminalSnapshot, PulseActionDelta, PulseTarget, SessionPolicy,
    SessionSlot, TerminalEvent, TerminalEventEnvelope, TerminalEventKind, TerminalRun,
    TerminalRunStatus, TerminalSlot, TerminalsSnapshot, ThinkingLevel, ToolKind,
};

use crate::agents::AgentsDashboardSnapshot;

pub(crate) const PROJECT: &str = "benchmark-project";

/// The three stages that stream; the dock follows the first.
pub(crate) const ROLES: [AgentRole; 3] = [
    AgentRole::BackendAgent,
    AgentRole::TestAgent,
    AgentRole::TaskSpecifier,
];

fn run(role: AgentRole) -> TerminalRun {
    TerminalRun {
        id: format!("benchmark-run-{}", role.as_str()),
        project_id: PROJECT.into(),
        slot: SessionSlot::Canonical { agent_role: role },
        slot_id: format!("canonical:{PROJECT}:{}", role.as_str()),
        agent_role: Some(role),
        origin: ExecutionOrigin::Pipeline,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Claude),
        model: Some("offline-fixture".into()),
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
        created_at: "2026-11-16T00:00:00Z".into(),
        started_at: Some("2026-11-16T00:00:00Z".into()),
        finished_at: None,
        updated_at: "2026-11-16T00:00:00Z".into(),
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

pub(crate) fn agent(role: AgentRole) -> AgentTerminalSnapshot {
    AgentTerminalSnapshot {
        config: CliAgentConfig {
            project_id: PROJECT.into(),
            agent_role: role,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Claude,
            model: Some("offline-fixture".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            custom_executable: None,
            custom_args: vec![],
            custom_risk_ack: false,
            cli_silence_timeout_ms: None,
            version: 1,
            created_at: "2026-11-16T00:00:00Z".into(),
            updated_at: "2026-11-16T00:00:00Z".into(),
        },
        recent_runs: vec![run(role)],
        recent_events: vec![],
    }
}

/// An idle free-form agent, for the canvas workload (feature 25, M1).
pub(crate) fn free_form(index: usize) -> FreeFormTerminalSnapshot {
    FreeFormTerminalSnapshot {
        slot: TerminalSlot {
            id: format!("benchmark-free-form-{index}"),
            project_id: PROJECT.into(),
            kind: "free_form".into(),
            agent_role: None,
            label: format!("Free-form {}", index + 1),
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
            position: u32::try_from(index).unwrap_or(u32::MAX),
            archived: false,
            version: 1,
            created_at: "2026-11-22T00:00:00Z".into(),
            updated_at: "2026-11-22T00:00:00Z".into(),
        },
        recent_runs: vec![],
        recent_events: vec![],
    }
}

/// One action of `role`'s run, as the coordinator records it: a call and,
/// on the next frame, its result.
pub(crate) fn envelope(role: AgentRole, frame: u64) -> TerminalEventEnvelope {
    let sequence = frame + 1;
    let ordinal = u32::try_from(frame / 2 + 1).unwrap_or(u32::MAX);
    let run = run(role);
    TerminalEventEnvelope {
        protocol_version: 2,
        project_id: PROJECT.into(),
        agent_role: Some(role),
        run_id: run.id.clone(),
        sequence,
        event: TerminalEvent {
            id: format!("{}-{sequence}", run.id),
            run_id: run.id.clone(),
            sequence,
            kind: TerminalEventKind::ToolUse,
            stream: None,
            content: format!("Read · src/module_{ordinal}.rs"),
            redaction_count: 0,
            redaction_categories: vec![],
            created_at: "2026-11-16T00:00:00Z".into(),
            detail: Some(EventDetailV1::ActionCall {
                v: 1,
                ordinal,
                kind: ToolKind::Read,
                name: "Read".into(),
            }),
            observed_at: None,
        },
        run,
    }
}

pub(crate) fn pulse(role: AgentRole, frame: u64) -> AgentActivityPulseV1 {
    let ordinal = u32::try_from(frame + 1).unwrap_or(u32::MAX);
    AgentActivityPulseV1 {
        protocol_version: 1,
        project_id: PROJECT.into(),
        run_id: run(role).id,
        agent_role: Some(role),
        slot_id: None,
        pulse_sequence: frame + 1,
        emitted_at: "2026-11-16T00:00:00.000Z".into(),
        phase: ActivityPhase::RunningTool,
        phase_since_ms: 0,
        last_activity_ms_ago: 0,
        actions: vec![PulseActionDelta::Started {
            ordinal,
            kind: ToolKind::Read,
            name: "Read".into(),
            target: Some(PulseTarget::WorkspacePath(format!(
                "src/module_{ordinal}.rs"
            ))),
        }],
        actions_dropped: 0,
        counters: ActivityCounters {
            tool_calls: ordinal,
            files_read: ordinal,
            elapsed_ms: frame * 16,
            ..ActivityCounters::default()
        },
    }
}

/// Frame 0 opens the dock on three running stages; every frame, each run
/// gets an event and a pulse. Returns the runs updated.
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
                    terminals: TerminalsSnapshot {
                        project_id: PROJECT.into(),
                        agents: ROLES.into_iter().map(agent).collect(),
                        free_form_slots: vec![],
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
    ROLES.len()
}
