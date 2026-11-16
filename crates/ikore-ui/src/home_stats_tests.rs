use super::*;
use ikore_core::persistence_service::PulsationStage;
use ikore_core::terminals::{
    AgentExecutionMode, AgentTerminalSnapshot, CliAgentConfig, CliBackend, CliModelSelectionSource,
    ExecutionOrigin, SessionSlot, ThinkingLevel,
};
use ikore_core::usage::{ModelUsage, UsageBucket, UsageTotals, UsageTransport};

fn at(value: &str) -> DateTime<Utc> {
    parse_utc(value).expect("fixture timestamp")
}

fn totals(input: i64, output: i64) -> UsageTotals {
    UsageTotals {
        input_tokens: Some(input),
        output_tokens: Some(output),
        total_tokens: Some(input + output),
        records: 1,
        ..Default::default()
    }
}

fn model(model_id: &str, transport: UsageTransport, tokens: i64) -> ModelUsage {
    ModelUsage {
        provider_id: "provider".into(),
        model_id: model_id.into(),
        model_source: "configured".into(),
        transport,
        totals: totals(tokens, 0),
    }
}

fn run(
    role: AgentRole,
    status: TerminalRunStatus,
    started: Option<&str>,
    finished: Option<&str>,
    pulsation: Option<&str>,
) -> TerminalRun {
    TerminalRun {
        id: format!("run-{}", started.unwrap_or("none")),
        project_id: "project".into(),
        slot: SessionSlot::Canonical { agent_role: role },
        slot_id: format!("canonical:project:{}", role.as_str()),
        agent_role: Some(role),
        origin: ExecutionOrigin::Pipeline,
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Codex),
        model: None,
        requested_model_profile: None,
        model_source: CliModelSelectionSource::Default,
        observed_model: None,
        native_session_id: None,
        pulsation_id: pulsation.map(Into::into),
        job_id: None,
        agent_run_id: None,
        llm_execution_id: None,
        status,
        exit_code: None,
        error_code: None,
        error_message: None,
        created_at: started.unwrap_or("2026-09-20T00:00:00Z").into(),
        started_at: started.map(Into::into),
        finished_at: finished.map(Into::into),
        updated_at: finished.unwrap_or("2026-09-20T00:00:00Z").into(),
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

fn snapshot(agents: Vec<(AgentRole, Vec<TerminalRun>)>) -> TerminalsSnapshot {
    TerminalsSnapshot {
        project_id: "project".into(),
        agents: agents
            .into_iter()
            .map(|(role, recent_runs)| AgentTerminalSnapshot {
                config: CliAgentConfig {
                    project_id: "project".into(),
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
                recent_runs,
                recent_events: vec![],
            })
            .collect(),
        free_form_slots: vec![],
        archived_free_form_slots: vec![],
        discovery: vec![],
    }
}

fn pulsation(id: &str, status: PulsationStatus, created: &str, updated: &str) -> Pulsation {
    Pulsation {
        terminal_reason: None,
        id: id.into(),
        project_id: "project".into(),
        feature_name: format!("Feature {id}"),
        stage: PulsationStage::Code,
        status,
        created_at: created.into(),
        updated_at: updated.into(),
    }
}

#[test]
fn a_week_of_tokens_is_seven_utc_days_with_the_gaps_at_zero() {
    let today = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
    let summary = UsageSummary {
        series: vec![
            UsageBucket {
                period: "2026-09-19".into(),
                totals: totals(100, 50),
            },
            UsageBucket {
                period: "2026-09-25".into(),
                totals: UsageTotals {
                    partial_records: 1,
                    ..totals(10, 5)
                },
            },
            // Outside the window: ignored.
            UsageBucket {
                period: "2026-09-18".into(),
                totals: totals(999, 999),
            },
        ],
        ..Default::default()
    };
    let days = token_days(Some(&summary), today);
    assert_eq!(days.len(), 7);
    assert_eq!(days[0].date.to_string(), "2026-09-19");
    assert_eq!(days[6].date, today);
    assert_eq!(
        (days[0].input, days[0].output, days[0].total),
        (100, 50, 150)
    );
    assert!(days[1..6].iter().all(|day| day.total == 0));
    assert!(days[6].incomplete && !days[0].incomplete);
    // No read yet: still seven empty days, so the chart keeps its shape.
    assert!(token_days(None, today).iter().all(|day| day.total == 0));
}

#[test]
fn model_shares_merge_transports_and_fold_the_tail_into_other() {
    let summary = UsageSummary {
        models: vec![
            model("gpt-5", UsageTransport::Api, 300),
            model("gpt-5", UsageTransport::Cli, 200),
            model("claude", UsageTransport::Cli, 400),
            model("a", UsageTransport::Api, 50),
            model("b", UsageTransport::Api, 40),
            model("c", UsageTransport::Api, 30),
            model("d", UsageTransport::Api, 20),
            model("e", UsageTransport::Api, 10),
            model("zero", UsageTransport::Api, 0),
        ],
        ..Default::default()
    };
    let shares = model_shares(Some(&summary));
    let labels: Vec<&str> = shares.iter().map(|share| share.label.as_str()).collect();
    assert_eq!(labels, ["gpt-5", "claude", "a", "b", "c", "Other"]);
    assert_eq!(shares[0].tokens, 500);
    assert_eq!(shares[5].tokens, 30);
    assert!(shares[5].other && !shares[0].other);
    assert!(model_shares(None).is_empty());
}

#[test]
fn agent_timings_use_finished_runs_of_the_last_week() {
    let now = at("2026-09-25T12:00:00Z");
    let snapshot = snapshot(vec![
        (
            AgentRole::TaskSpecifier,
            vec![
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Completed,
                    Some("2026-09-25T10:00:00Z"),
                    Some("2026-09-25T10:01:00Z"),
                    None,
                ),
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Failed,
                    Some("2026-09-24T10:00:00Z"),
                    Some("2026-09-24T10:05:00Z"),
                    None,
                ),
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Completed,
                    Some("2026-09-23T10:00:00Z"),
                    Some("2026-09-23T10:03:00Z"),
                    None,
                ),
                // Too old, and still running: both left out.
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Completed,
                    Some("2026-09-10T10:00:00Z"),
                    Some("2026-09-10T11:00:00Z"),
                    None,
                ),
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Running,
                    Some("2026-09-25T11:59:00Z"),
                    None,
                    None,
                ),
            ],
        ),
        (AgentRole::BackendAgent, vec![]),
    ]);
    let rows = agent_timings(Some(&snapshot), now);
    assert_eq!(rows.len(), 1, "an agent without a finished run is left out");
    let row = &rows[0];
    assert_eq!(row.role, AgentRole::TaskSpecifier);
    assert_eq!((row.runs, row.failed), (3, 1));
    assert_eq!(row.median_secs, 180);
    assert_eq!(row.longest_secs, 300);
}

#[test]
fn pulsation_timings_keep_the_newest_started_ones_oldest_first() {
    let now = at("2026-09-25T12:00:00Z");
    let mut list: Vec<Pulsation> = (0..10)
        .map(|index| {
            pulsation(
                &format!("p{index}"),
                PulsationStatus::Done,
                &format!("2026-09-{:02}T10:00:00Z", 10 + index),
                &format!("2026-09-{:02}T10:{:02}:00Z", 10 + index, index),
            )
        })
        .collect();
    list.push(pulsation(
        "running",
        PulsationStatus::Running,
        "2026-09-25T11:30:00Z",
        "2026-09-25T11:31:00Z",
    ));
    list.push(pulsation(
        "idle",
        PulsationStatus::Idle,
        "2026-09-25T11:50:00Z",
        "2026-09-25T11:50:00Z",
    ));
    let rows = pulsation_timings(&list, now);
    assert_eq!(rows.len(), PULSATION_BARS);
    assert_eq!(rows.first().unwrap().id, "p3");
    assert_eq!(
        rows.last().unwrap().id,
        "running",
        "idle pulsations have no duration"
    );
    // A running pulsation counts until now, a finished one until its last update.
    assert_eq!(rows.last().unwrap().secs, 30 * 60);
    assert!(!rows.last().unwrap().finished);
    assert_eq!(rows[0].secs, 3 * 60);
    // p3..p9 finished in 3..9 minutes: the median is 6.
    assert_eq!(typical_pulsation_secs(&rows), Some(6 * 60));
}

#[test]
fn stage_times_sum_each_agent_in_the_order_they_started() {
    let now = at("2026-09-25T12:00:00Z");
    let snapshot = snapshot(vec![
        (
            AgentRole::BackendAgent,
            vec![run(
                AgentRole::BackendAgent,
                TerminalRunStatus::Running,
                Some("2026-09-25T11:50:00Z"),
                None,
                Some("p1"),
            )],
        ),
        (
            AgentRole::TaskSpecifier,
            vec![
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Completed,
                    Some("2026-09-25T11:00:00Z"),
                    Some("2026-09-25T11:02:00Z"),
                    Some("p1"),
                ),
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Completed,
                    Some("2026-09-25T11:10:00Z"),
                    Some("2026-09-25T11:13:00Z"),
                    Some("p1"),
                ),
                // Another pulsation: not counted.
                run(
                    AgentRole::TaskSpecifier,
                    TerminalRunStatus::Completed,
                    Some("2026-09-25T09:00:00Z"),
                    Some("2026-09-25T09:30:00Z"),
                    Some("p0"),
                ),
            ],
        ),
    ]);
    assert_eq!(
        stage_times(Some(&snapshot), "p1", now),
        vec![
            (AgentRole::TaskSpecifier, 300),
            (AgentRole::BackendAgent, 600)
        ]
    );
}

#[test]
fn durations_and_timestamps_read_the_way_people_write_them() {
    assert_eq!(format_secs(45), "45s");
    assert_eq!(format_secs(192), "3m 12s");
    assert_eq!(format_secs(3_840), "1h 04m");
    assert_eq!(format_secs(183_600), "2d 3h");
    assert_eq!(
        parse_utc("2026-09-25 10:00:00"),
        parse_utc("2026-09-25T10:00:00Z")
    );
    assert_eq!(
        parse_utc("2026-09-25T07:00:00-03:00"),
        parse_utc("2026-09-25T10:00:00Z")
    );
    assert!(parse_utc("yesterday").is_none());
}
