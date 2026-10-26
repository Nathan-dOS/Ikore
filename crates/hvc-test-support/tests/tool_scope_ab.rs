#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! ADR-034 T1 (D3): real-run measurement of the context scope of Intake,
//! Contract and the two repairers.
//!
//! Paid and slow: it runs the real `claude` CLI found on PATH (Haiku) for
//! Intake, Contract, Code and Test; Specification and the Integrator stay on
//! the scripted provider. The test measures whatever the binary was built
//! with. Build it once with the current policy tables (arm `workspace`) and
//! once with `HostContextOnly` for the four executors (arm `host_context`),
//! then run both binaries at the same time:
//! `HVC_T1_ARM=<label> HVC_T1_OUT=<jsonl> <binary> --ignored --nocapture`.
mod composition_support;

use composition_support::*;
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_test_support::IsolatedTestEnvironment;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const RUN_DEADLINE: Duration = Duration::from_secs(25 * 60);
const EXECUTORS: [&str; 4] = [
    "intake_classifier",
    "contract_author",
    "code_repairer",
    "test_repairer",
];

/// Approves every artifact review until the pulsation ends; returns
/// `(status, stage, approvals)`.
fn drive(harness: &Harness) -> (String, String, usize) {
    let deadline = Instant::now() + RUN_DEADLINE;
    let mut approvals = 0;
    loop {
        let (stage, status): (String, String) = harness
            .db()
            .query_row(
                "SELECT stage,status FROM pulsations WHERE id=?1",
                [&harness.pulse],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        match status.as_str() {
            "done" | "failed" => return (status, stage, approvals),
            "checkpoint" => {
                let kind: Option<String> = harness
                    .db()
                    .query_row(
                        "SELECT kind FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' ORDER BY created_at DESC LIMIT 1",
                        [&harness.pulse],
                        |r| r.get(0),
                    )
                    .ok();
                if kind.as_deref() == Some("artifact_review") && approvals < 12 {
                    harness.approve();
                    approvals += 1;
                } else {
                    return (
                        format!("checkpoint:{}", kind.unwrap_or_default()),
                        stage,
                        approvals,
                    );
                }
            }
            _ => {}
        }
        if Instant::now() > deadline {
            return ("timeout".into(), stage, approvals);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn executor_metrics(harness: &Harness) -> Vec<Value> {
    let db = harness.db();
    let mut statement = db
        .prepare(
            "SELECT executor_kind,
                    COUNT(DISTINCT unit_id),
                    COALESCE(SUM(ordinal = 1 AND state = 'succeeded'),0),
                    COALESCE(SUM(ordinal > 1),0),
                    COALESCE(SUM(state = 'succeeded'),0),
                    COUNT(*),
                    GROUP_CONCAT(DISTINCT error_code)
             FROM stage_work_unit_attempts
             GROUP BY executor_kind",
        )
        .unwrap();
    statement
        .query_map([], |r| {
            Ok(json!({
                "executor": r.get::<_, String>(0)?,
                "units": r.get::<_, i64>(1)?,
                "firstAttemptOk": r.get::<_, i64>(2)?,
                "retries": r.get::<_, i64>(3)?,
                "succeeded": r.get::<_, i64>(4)?,
                "attempts": r.get::<_, i64>(5)?,
                "errors": r.get::<_, Option<String>>(6)?,
            }))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
#[ignore = "paid: runs the real Claude CLI (Haiku) for Intake, Contract, Code and Test"]
fn context_scope_real_run() {
    let arm = std::env::var("HVC_T1_ARM").unwrap_or_else(|_| "unlabelled".into());
    let run: usize = std::env::var("HVC_T1_RUN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let env = IsolatedTestEnvironment::new();
    std::fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    std::fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    let started = Instant::now();
    let harness = Harness::start_prepared(
        env,
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            for role in [
                "intake_router",
                "contract_agent",
                "backend_agent",
                "test_agent",
            ] {
                harness.route_role_to_official_cli(role, "claude");
            }
        },
    );
    let (status, stage, approvals) = drive(&harness);
    let line = json!({
        "arm": arm,
        "run": run,
        "status": status,
        "stoppedAtStage": stage,
        "approvals": approvals,
        "wallSeconds": started.elapsed().as_secs(),
        "executors": executor_metrics(&harness),
        "cliRuns": harness
            .db()
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(u.input_tokens),0), COALESCE(SUM(u.output_tokens),0)
                 FROM terminal_runs r LEFT JOIN token_usage_records u ON u.source_id=r.id
                 WHERE r.pulsation_id=?1",
                [&harness.pulse],
                |r| Ok(json!({"runs": r.get::<_, i64>(0)?, "inputTokens": r.get::<_, i64>(1)?, "outputTokens": r.get::<_, i64>(2)?})),
            )
            .unwrap(),
        "trackedExecutors": EXECUTORS,
    });
    println!("{line}");
    if let Ok(path) = std::env::var("HVC_T1_OUT") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }
}
