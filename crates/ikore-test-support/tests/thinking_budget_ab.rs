#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! Paired A/B of the Claude thinking budget on the stages that think the most.
//!
//! Paid and slow: it runs the real `claude` CLI found on PATH (Haiku) for
//! Specification and Contract, twice per pair — the role's default thinking
//! against `thinking_level = low` (`MAX_THINKING_TOKENS=4096`). Both arms of a
//! pair run at the same time on the same batch, so they share provider load.
//! Intake stays on the scripted provider so both arms start from the same
//! classification. Run with
//! `cargo test -p ikore-test-support --test thinking_budget_ab -- --ignored --nocapture`;
//! `HVC_AB_PAIRS` sets the number of pairs (default 3) and `HVC_AB_FIRST_PAIR`
//! the number of the first one. One JSON line per arm
//! is printed and appended to `HVC_AB_OUT` when set.
mod composition_support;

use composition_support::*;
use ikore_core::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind};
use ikore_core::orchestration::composition::settings::CompositionSettings;
use ikore_test_support::IsolatedTestEnvironment;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Generous: a Specification alone took 150 s in the analysed pulsation.
const STAGE_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// The batch of "Haiku CLI teste 1" (`ToDoListWebView`, 2026-09-26), without
/// its task dependencies and feature link.
fn todo_task() -> TaskDraft {
    let criteria = [
        "Users can create, edit, schedule, and prioritize tasks; no completion/done state is included.",
        "Month and week views display scheduled tasks, and selecting a day opens its prioritized task list.",
        "Task data is stored locally and survives browser reloads.",
        "The interface loads in under 1 second in Firefox desktop and Firefox for Android.",
        "Users can configure the calendar start day, with 60-minute time slots/task duration.",
    ];
    TaskDraft {
        title: "Build local task management and calendar views (index.html, src/js/app.js, src/js/views/calendarView.js, src/js/views/dayDetailView.js, src/js/storage/localStore.js)".into(),
        kind: TaskKind::Feature,
        description: "Implement the agreed MVP flows for creating, editing, scheduling, prioritizing, and viewing personal tasks in month and week calendars, including the prioritized day-detail window and the configurable calendar start day setting with fixed 60-minute time slots. No completion/done flow is included. Files: `index.html` (app shell), `src/js/app.js` (bootstrap/orchestration), `src/js/views/calendarView.js` (month/week views), `src/js/views/dayDetailView.js` (day-detail prioritized list), `src/js/storage/localStore.js` (local persistence).".into(),
        criteria: criteria
            .iter()
            .enumerate()
            .map(|(index, description)| AcceptanceCriterion {
                id: format!("T3.{}", index + 1),
                description: (*description).into(),
            })
            .collect(),
        dependencies: vec![],
        feature_id: None,
    }
}

/// Waits for the pulsation to stop at any checkpoint or end, and returns
/// `(stage, status, checkpoint kind)`.
fn wait_for_stop(harness: &Harness) -> (String, String, Option<String>) {
    let db = harness.db();
    let deadline = Instant::now() + STAGE_DEADLINE;
    loop {
        let (stage, status): (String, String) = db
            .query_row(
                "SELECT stage,status FROM pulsations WHERE id=?1",
                [&harness.pulse],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if matches!(status.as_str(), "checkpoint" | "failed" | "done") {
            let kind = db
                .query_row(
                    "SELECT kind FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' ORDER BY created_at DESC LIMIT 1",
                    [&harness.pulse],
                    |r| r.get::<_, String>(0),
                )
                .ok();
            return (stage, status, kind);
        }
        assert!(
            Instant::now() < deadline,
            "stage {stage} did not stop in time"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Thinking tokens across every thinking block of a run: Claude reports a
/// running estimate per block that restarts with the next block.
fn thinking_tokens(db: &Connection, run: &str) -> i64 {
    let mut statement = db
        .prepare("SELECT content FROM terminal_events WHERE run_id=?1 AND content LIKE '%\"subtype\":\"thinking_tokens\"%' ORDER BY sequence")
        .unwrap();
    let estimates = statement
        .query_map([run], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .filter_map(|content| {
            let json = &content[content.find('{')?..];
            serde_json::from_str::<Value>(json).ok()?["estimated_tokens"].as_i64()
        });
    let (mut total, mut block) = (0, 0);
    for estimate in estimates {
        if estimate < block {
            total += block;
        }
        block = estimate;
    }
    total + block
}

/// One row per Claude run of the stage: time, tokens, thinking, attempt state.
fn stage_metrics(harness: &Harness, stage: &str) -> Vec<Value> {
    let db = harness.db();
    let mut statement = db
        .prepare(
            "SELECT r.id,
                    (julianday(r.finished_at)-julianday(r.started_at))*86400,
                    COALESCE(u.input_tokens,0), COALESCE(u.output_tokens,0),
                    COALESCE(a.state,'?'), a.error_code, substr(a.diagnostic,1,400)
             FROM terminal_runs r
             JOIN stage_work_unit_attempts a ON a.agent_run_id=r.agent_run_id
             JOIN stage_executions e ON e.id=a.execution_id
             LEFT JOIN token_usage_records u ON u.source_id=r.id
             WHERE r.pulsation_id=?1 AND e.stage=?2
             ORDER BY r.created_at",
        )
        .unwrap();
    let rows = statement
        .query_map(rusqlite::params![harness.pulse, stage], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    rows.into_iter()
        .map(|(run, seconds, input, output, state, error, diagnostic)| {
            json!({
                "seconds": (seconds * 10.0).round() / 10.0,
                "inputTokens": input,
                "outputTokens": output,
                "thinkingTokens": thinking_tokens(&db, &run),
                "attempt": state,
                "error": error,
                "diagnostic": diagnostic,
            })
        })
        .collect()
}

fn run_arm(pair: usize, arm: &'static str) -> Value {
    let harness = Harness::start_prepared(
        IsolatedTestEnvironment::new(),
        vec![todo_task()],
        CompositionSettings {
            enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            for role in ["task_specifier", "contract_agent"] {
                harness.route_role_to_official_cli(role, "claude");
            }
            let level = if arm == "low" { "low" } else { "default" };
            harness
                .db()
                .execute(
                    "UPDATE cli_agent_configs SET thinking_level=?2,version=version+1 WHERE project_id=?1 AND agent_role IN ('task_specifier','contract_agent')",
                    rusqlite::params![harness.project, level],
                )
                .unwrap();
        },
    );
    let mut stages = serde_json::Map::new();
    let mut outcome = "contract_checkpoint";
    for stage in ["SPECIFICATION", "Contract"] {
        let started = Instant::now();
        let (stopped_stage, status, kind) = wait_for_stop(&harness);
        stages.insert(
            stage.into(),
            json!({
                "wallSeconds": started.elapsed().as_secs(),
                "stoppedAt": stopped_stage,
                "status": status,
                "checkpoint": kind,
                "runs": stage_metrics(&harness, stage),
            }),
        );
        // Only a clean artifact review of this very stage continues the arm.
        if stopped_stage != stage || kind.as_deref() != Some("artifact_review") {
            outcome = "stopped_early";
            break;
        }
        if stage == "SPECIFICATION" {
            harness.approve();
        }
    }
    let line = json!({"pair": pair, "arm": arm, "outcome": outcome, "stages": stages});
    println!("{line}");
    if let Ok(path) = std::env::var("HVC_AB_OUT") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }
    line
}

#[test]
#[ignore = "paid: runs the real Claude CLI (Haiku) for Specification and Contract"]
fn claude_thinking_budget_paired_ab_on_specification_and_contract() {
    let number = |name: &str, default: usize| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let (pairs, first) = (number("HVC_AB_PAIRS", 3), number("HVC_AB_FIRST_PAIR", 1));
    for pair in first..first + pairs {
        std::thread::scope(|scope| {
            let default = scope.spawn(move || run_arm(pair, "default"));
            let low = scope.spawn(move || run_arm(pair, "low"));
            default.join().unwrap();
            low.join().unwrap();
        });
    }
}
