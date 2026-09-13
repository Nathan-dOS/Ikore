//! Pipeline v4 over the CLI transport (ADR-022): the Code and Test roles run
//! through a `cli_process` backend (the `cli_fixture` binary in bridge mode)
//! while the other stages stay on the API. The same scripted responder
//! serves both transports, so the test can compare what each one received.
mod composition_support;

use composition_support::*;
use serde_json::Value;
use std::fs;

#[test]
fn composed_stages_run_over_the_cli_transport_with_the_schema_delivered_once() {
    let harness = Harness::start(|facts, _, request| {
        let body: Value = serde_json::from_str(&request.body).unwrap();
        if let Some(bridge) = body.get("bridge") {
            // The CLI prompt carries the response schema exactly once (the
            // transport appends it for Custom backends) and the projection
            // itself never embeds it.
            assert_eq!(bridge["schema_mentions"], 1, "{}", facts.executor);
            assert!(!facts.user_prompt.contains("\"$schema\""));
            assert!(
                bridge["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a == "--bridge-url"),
                "custom args reach the executable"
            );
        }
        completed_reply(facts)
    });
    harness.route_role_to_cli_bridge("backend_agent");
    harness.route_role_to_cli_bridge("test_agent");

    for stage in ["SPECIFICATION", "Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "test_author",
            "integration_reviewer"
        ]
    );

    let db = harness.db();
    let pulse = &harness.pulse;
    // The effective profile of each attempt records the transport it used,
    // and the CLI attempts are the ones with a terminal run bound to their
    // own agent run - the transport is per specialty, never per pulsation.
    let profiles: Vec<(String, String, String, i64)> = db
        .prepare("SELECT a.executor_kind,json_extract(s.snapshot_json,'$.transport'),json_extract(s.snapshot_json,'$.backend'),(SELECT COUNT(*) FROM terminal_runs t WHERE t.agent_run_id=a.agent_run_id) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id JOIN composition_policy_snapshots s ON s.id=a.profile_snapshot_id WHERE e.pulsation_id=?1 AND a.state='succeeded' ORDER BY a.created_at")
        .unwrap()
        .query_map([pulse], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get(3)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(profiles.len(), 6, "{profiles:?}");
    for (kind, transport, backend, runs) in &profiles {
        let cli = matches!(kind.as_str(), "code_implementer" | "test_author");
        assert_eq!(transport, if cli { "cli" } else { "api" }, "{kind}");
        assert_eq!(*runs, i64::from(cli), "{kind} terminal runs");
        if cli {
            assert_eq!(backend, "custom", "{kind}");
        }
    }
    let settled: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_invocation_reservations r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND r.state='settled'",
            [pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(settled, 6, "CLI permits settle like API permits");

    // Both transports end in the same host-reconstructed files.
    let workspace = harness.env.workspace_dir();
    assert_eq!(
        fs::read_to_string(workspace.join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE
    );
    assert_eq!(
        fs::read_to_string(workspace.join("tests/auth_test.rs")).unwrap(),
        AUTHORED_TEST
    );
}
