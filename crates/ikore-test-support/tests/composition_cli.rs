//! Pipeline v4 over the CLI transport (ADR-022): the Code and Test roles run
//! through a `cli_process` backend (the `cli_fixture` binary in bridge mode)
//! while the other stages stay on the API. The same scripted responder
//! serves both transports, so the test can compare what each one received.
mod composition_support;

use composition_support::*;
use serde_json::Value;
use std::fs;
use std::sync::{Mutex, OnceLock};

static PATH_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

#[test]
fn claude_haiku_selection_parks_the_pulsation_for_subagent_mismatch_or_missing_identity() {
    let _path_lock = PATH_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap();
    let binaries = tempfile::tempdir().unwrap();
    let claude = binaries.path().join("claude.exe");
    fs::copy(env!("CARGO_BIN_EXE_cli_fixture"), &claude).unwrap();
    let original_path = std::env::var_os("PATH");
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path.as_ref() {
        paths.extend(std::env::split_paths(original_path));
    }
    // SAFETY: this integration binary serializes its only PATH mutation and restores it below.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    struct RestorePath(Option<std::ffi::OsString>);
    impl Drop for RestorePath {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                // SAFETY: the test holds PATH_TEST_LOCK for the lifetime of the guard.
                unsafe { std::env::set_var("PATH", path) };
            } else {
                // SAFETY: the test holds PATH_TEST_LOCK for the lifetime of the guard.
                unsafe { std::env::remove_var("PATH") };
            }
        }
    }
    let _restore_path = RestorePath(original_path);

    for (state, expected_code, expected_model) in [
        (
            "subagent_sonnet_mismatch",
            "model_mismatch",
            Some("claude-sonnet-5"),
        ),
        ("missing_generation_identity", "model_unverified", None),
    ] {
        fs::write(binaries.path().join("claude.fixture-state"), state).unwrap();
        let marker = binaries.path().join("claude.fixture-after-mismatch");
        let _ = fs::remove_file(&marker);
        let harness = Harness::start(|facts, _, _| completed_reply(facts));
        harness.route_role_to_official_cli("backend_agent", "claude");
        for stage in ["SPECIFICATION", "Contract"] {
            harness.wait_for_checkpoint(stage);
            harness.approve();
        }

        let checkpoint = harness.wait_for_failure_checkpoint();
        let db = harness.db();
        let (failure_class, error_code, allow_retry): (String, String, bool) = db
            .query_row(
                "SELECT failure_class,error_code,allow_retry FROM stage_failure_requests WHERE checkpoint_id=?1",
                [&checkpoint],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(failure_class, "host_contract");
        assert_eq!(error_code, expected_code);
        assert!(
            !allow_retry,
            "a model identity violation cannot be retried automatically"
        );
        assert_eq!(
            harness.calls(),
            [
                "intake_classifier",
                "specification_author",
                "contract_author"
            ]
        );
        let terminal_runs: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM terminal_runs WHERE pulsation_id=?1",
                [&harness.pulse],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            terminal_runs, 1,
            "the failed Code call has no automatic retry"
        );
        assert_eq!(
            fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
            ORIGINAL_SOURCE,
            "a divergent generation must not be published"
        );
        assert!(
            !marker.exists(),
            "the host terminates the Claude process tree at the first unverified generation"
        );
        type GenerationEvidence = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let generations: Vec<GenerationEvidence> = db
            .prepare(
                "SELECT o.model_id,o.message_id,o.parent_invocation_id,o.agent_id FROM terminal_model_observations o JOIN terminal_runs t ON t.id=o.run_id WHERE t.pulsation_id=?1 AND o.is_generation=1 ORDER BY o.ordinal",
            )
            .unwrap()
            .query_map([&harness.pulse], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(generations.len(), 1);
        assert_eq!(generations[0].0.as_deref(), expected_model);
        assert_eq!(generations[0].1.as_deref(), Some("claude-message-1"));
        assert_eq!(generations[0].2.as_deref(), Some("tool-parent-1"));
        assert_eq!(generations[0].3.as_deref(), Some("subagent-1"));
    }
}
