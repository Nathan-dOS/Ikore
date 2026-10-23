#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
//! Pipeline v4 bounded parallel engine (ADR-022, M6): independent units of
//! a stage run together up to the frozen cap, the round settles before any
//! decision, outcomes apply in plan order, a failed sibling parks the
//! execution once, and cancellation reaches every unit in flight. The
//! parallel result must equal the sequential one (parity gate).
mod composition_support;

use composition_support::*;
use hvc_core::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind};
use hvc_core::orchestration::composition::settings::CompositionSettings;
use hvc_core::persistence_service::{
    CancelPulsationJobsRequest, ResolveStageFailureRequest, StageFailureDecision,
};
use hvc_test_support::{IsolatedTestEnvironment, ScriptedResponse};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SESSION_SOURCE: &str = "pub fn open_session(expires_at: u64, now: u64) -> bool {\n    let _ = (expires_at, now);\n    true\n}\n";
const SESSION_IMPLEMENTED: &str =
    "pub fn open_session(expires_at: u64, now: u64) -> bool {\n    expires_at > now\n}\n";

fn session_task() -> TaskDraft {
    TaskDraft {
        title: "Sessions reject expired tokens".into(),
        kind: TaskKind::Bug,
        description:
            "open_session in src/session.rs must compare the expiry; add tests/session_test.rs"
                .into(),
        criteria: vec![AcceptanceCriterion {
            id: "ac-2".into(),
            description: "open_session returns false for an expired token".into(),
        }],
        dependencies: vec![],
        feature_id: None,
    }
}

fn fixture_env() -> IsolatedTestEnvironment {
    let env = IsolatedTestEnvironment::new();
    fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    fs::write(env.workspace_dir().join("src/session.rs"), SESSION_SOURCE).unwrap();
    env
}

/// Two code units and two test units with no dependency between them:
/// the session unit reads and writes only its own file.
fn proposals(facts: &PromptFacts) -> Value {
    let auth = facts.handle_for_path("src/auth.rs");
    let session = facts.handle_for_path("src/session.rs");
    let auth_test = facts.handle_for_path("tests/auth_test.rs");
    let session_test = facts.handle_for_path("tests/session_test.rs");
    json!([
        {"unitKey": "implement_validator", "kind": "code", "taskRefs": ["task_1"], "criterionRefs": ["crit_1_1"],
         "inputRefs": [auth], "destinationRefs": [auth], "dependsOn": [], "objective": "Compare the expiry against the clock."},
        {"unitKey": "implement_session", "kind": "code", "taskRefs": ["task_2"], "criterionRefs": ["crit_2_1"],
         "inputRefs": [session], "destinationRefs": [session], "dependsOn": [], "objective": "Compare the session expiry against the clock."},
        {"unitKey": "test_expiry", "kind": "test", "taskRefs": ["task_1"], "criterionRefs": ["crit_1_1"],
         "inputRefs": [auth], "destinationRefs": [auth_test], "dependsOn": [], "objective": "Prove expired tokens are rejected."},
        {"unitKey": "test_session", "kind": "test", "taskRefs": ["task_2"], "criterionRefs": ["crit_2_1"],
         "inputRefs": [session], "destinationRefs": [session_test], "dependsOn": [], "objective": "Prove sessions reject expired tokens."}
    ])
}

fn options() -> ReplyOptions {
    let mut options = ReplyOptions::default();
    options
        .contents
        .insert("src/session.rs".into(), SESSION_IMPLEMENTED.into());
    options.contents.insert(
        "tests/session_test.rs".into(),
        "#[test]\nfn session_rejects_expired() {\n    assert!(!session::open_session(1, 2));\n}\n"
            .into(),
    );
    options
}

fn reply(facts: &PromptFacts) -> ScriptedResponse {
    let mut options = options();
    if facts.executor == "specification_author" {
        options.proposals = Some(proposals(facts));
    }
    provider_reply(unit_response_with(facts, &options))
}

/// Whether this is the first provider call of `executor` in the run - the
/// one that can observe its sibling being dispatched in the same round.
fn first_call_of(
    seen: &std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    executor: &str,
) -> bool {
    let mut seen = seen.lock().unwrap();
    let count = seen.entry(executor.to_owned()).or_insert(0);
    *count += 1;
    *count == 1
}

fn parallel_settings() -> CompositionSettings {
    CompositionSettings {
        enabled: true,
        max_in_flight_units: 2,
        ..CompositionSettings::default()
    }
    .with_all_recipes_qualified()
}

/// Blocks the fixture provider until `expected` attempts of the stage that
/// is being answered are durably `running` - i.e. the host dispatched the
/// sibling before this reply. Times out rather than hanging the suite.
fn wait_for_running_attempts(db_path: &Path, executor: &str, expected: i64) -> i64 {
    let db = Connection::open(db_path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let running: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM stage_work_unit_attempts WHERE executor_kind=?1 AND state='running'",
                [executor],
                |r| r.get(0),
            )
            .unwrap();
        if running >= expected || Instant::now() > deadline {
            return running;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The observable result of a stage: accepted artifacts and evidence, and
/// the files the workspace ends with.
struct StageResult {
    artifacts: Vec<(String, String, Option<String>)>,
    evidence: Vec<(String, String)>,
    auth: String,
    session: String,
}

fn stage_result(harness: &Harness) -> StageResult {
    let db = harness.db();
    let artifacts = db
        // The typed payload carries the plan digest (fresh ids per run), so
        // it is compared by its unit responses, in plan order.
        .prepare("SELECT kind,COALESCE(relative_path,''),CASE WHEN kind='stage_payload_v1' THEN json_extract(content,'$.units') ELSE content_sha256 END FROM artifacts WHERE pulsation_id=?1 AND acceptance='accepted' AND producer_stage IN ('Code','Test') ORDER BY producer_stage,kind,relative_path")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let evidence = db
        // Task ids are fresh per run; evidence is keyed by stage and criterion.
        .prepare("SELECT stage||':'||json_extract(evidence_json,'$.criterionIds[0]'),json_remove(evidence_json,'$.taskId') FROM task_evidence WHERE pulsation_id=?1 AND stage IN ('Code','Test') ORDER BY 1")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let workspace = harness.workspace();
    let auth = fs::read_to_string(workspace.join("src/auth.rs")).unwrap();
    let session = fs::read_to_string(workspace.join("src/session.rs")).unwrap();
    StageResult {
        artifacts,
        evidence,
        auth,
        session,
    }
}

fn run_to_done(harness: &Harness) {
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
}

#[test]
fn independent_units_run_together_and_match_the_sequential_result() {
    // Sequential reference (cap 1).
    let sequential = Harness::start_in(
        fixture_env(),
        vec![expiry_task(), session_task()],
        |facts, _, _| reply(facts),
    );
    run_to_done(&sequential);
    let reference = stage_result(&sequential);
    assert_eq!(sequential.calls().len(), 8, "{:?}", sequential.calls());

    // Parallel run (cap 2): every Code and Test reply waits until the
    // sibling attempt is durably running, so both were dispatched together.
    let env = fixture_env();
    let db_path: PathBuf = env.data_dir().join("hard-vibe-code.db");
    let seen = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let parallel = Harness::start_with(
        env,
        vec![expiry_task(), session_task()],
        parallel_settings(),
        move |facts, _, _| {
            if matches!(facts.executor.as_str(), "code_implementer" | "test_author")
                && first_call_of(&seen, &facts.executor)
            {
                let running = wait_for_running_attempts(&db_path, &facts.executor, 2);
                assert_eq!(
                    running, 2,
                    "{}: the sibling was dispatched in the same round",
                    facts.executor
                );
            }
            reply(facts)
        },
    );
    run_to_done(&parallel);
    assert_eq!(
        parallel.calls().len(),
        8,
        "no coordination call: {:?}",
        parallel.calls()
    );
    let result = stage_result(&parallel);
    assert_eq!(
        result.artifacts, reference.artifacts,
        "same accepted artifacts"
    );
    assert_eq!(result.evidence, reference.evidence, "same evidence rows");
    assert_eq!(result.auth, reference.auth);
    assert_eq!(result.session, reference.session);
    assert_eq!(result.auth, IMPLEMENTED_SOURCE);
    assert_eq!(result.session, SESSION_IMPLEMENTED);

    let db = parallel.db();
    let caps: Vec<(String, i64, i64)> = db
        .prepare("SELECT stage,json_extract(plan_json,'$.budget.maxInFlightUnits'),(SELECT COUNT(*) FROM stage_work_unit_attempts a WHERE a.execution_id=e.id AND a.state='succeeded') FROM stage_executions e WHERE pulsation_id=?1 AND stage IN ('Code','Test') ORDER BY created_at")
        .unwrap()
        .query_map([&parallel.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        caps,
        [("Code".to_owned(), 2, 2), ("Test".to_owned(), 2, 2)],
        "{caps:?}"
    );
    let pools: Vec<String> = db
        .prepare("SELECT r.pool FROM stage_invocation_reservations r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code'")
        .unwrap()
        .query_map([&parallel.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        pools,
        ["initial", "initial"],
        "concurrent first attempts each hold an initial permit"
    );
}

#[test]
fn a_failed_sibling_parks_once_after_the_round_settles_and_is_reused_on_retry() {
    let env = fixture_env();
    let db_path: PathBuf = env.data_dir().join("hard-vibe-code.db");
    let seen = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let harness = Harness::start_with(
        env,
        vec![expiry_task(), session_task()],
        parallel_settings(),
        move |facts, _, _| {
            if facts.executor == "code_implementer" {
                // Both code units are in flight; the session one fails at the
                // provider (configuration fault: no resample) while the
                // validator completes normally.
                if first_call_of(&seen, "code_implementer") {
                    let running = wait_for_running_attempts(&db_path, "code_implementer", 2);
                    assert_eq!(running, 2);
                }
                if facts
                    .owned()
                    .iter()
                    .any(|(_, row)| row.path == "src/session.rs")
                {
                    let db = Connection::open(&db_path).unwrap();
                    let prior: i64 = db
                        .query_row(
                            "SELECT COUNT(*) FROM stage_work_unit_attempts WHERE executor_kind='code_implementer' AND state IN ('rejected', 'cancelled')",
                            [],
                            |r| r.get(0),
                        )
                        .unwrap();
                    if prior == 0 {
                        return ScriptedResponse::json(
                            401,
                            r#"{"error":{"message":"fixture key rejected"}}"#,
                        );
                    }
                }
            }
            reply(facts)
        },
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    let checkpoint = harness.wait_for_failure_checkpoint();
    let db = harness.db();
    // The round settled before parking: the validator's result is durable
    // and the execution parked exactly once.
    let units: Vec<(String, String)> = db
        .prepare("SELECT u.logical_unit_key,u.state FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' AND e.status='parked' ORDER BY u.ordinal")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        units,
        [
            ("implement_validator".to_owned(), "validated".to_owned()),
            ("implement_session".to_owned(), "failed".to_owned())
        ]
    );
    let checkpoints: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_failure_requests WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(checkpoints, 1, "one park for the round");

    harness
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: checkpoint,
            expected_state_version: harness.state_version(),
            decision: StageFailureDecision::RetryStage,
            feedback: None,
        })
        .unwrap();
    for stage in ["Test", "Integrator"] {
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
    let reused: Vec<(String, Option<String>, i64)> = db
        .prepare("SELECT u.logical_unit_key,u.reused_from_attempt_id,(SELECT COUNT(*) FROM stage_invocation_reservations r WHERE r.unit_id=u.id) FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' AND e.status='accepted' ORDER BY u.ordinal")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(reused.len(), 2, "{reused:?}");
    assert!(
        reused[0].1.is_some() && reused[0].2 == 0,
        "validator reused without a permit: {reused:?}"
    );
    assert!(
        reused[1].1.is_none() && reused[1].2 == 1,
        "session ran once more: {reused:?}"
    );
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/session.rs")).unwrap(),
        SESSION_IMPLEMENTED
    );
}

#[test]
fn cancelling_the_pulsation_reaches_every_unit_in_flight_and_nothing_is_accepted() {
    let env = fixture_env();
    let db_path: PathBuf = env.data_dir().join("hard-vibe-code.db");
    let cancel = std::sync::Arc::new(std::sync::Mutex::new(
        None::<hvc_core::persistence_service::PersistenceService>,
    ));
    let cancel_handle = cancel.clone();
    let seen = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let harness = Harness::start_with(
        env,
        vec![expiry_task(), session_task()],
        parallel_settings(),
        move |facts, _, _| {
            if facts.executor == "code_implementer" && first_call_of(&seen, "code_implementer") {
                let running = wait_for_running_attempts(&db_path, "code_implementer", 2);
                assert_eq!(running, 2);
                // The developer cancels while both units are in flight; the
                // reply that follows is a late result.
                if let Some(service) = cancel_handle.lock().unwrap().as_ref() {
                    let pulse: String = Connection::open(&db_path)
                        .unwrap()
                        .query_row("SELECT id FROM pulsations LIMIT 1", [], |r| r.get(0))
                        .unwrap();
                    let _ = service.cancel_pulsation_jobs(CancelPulsationJobsRequest {
                        pulsation_id: pulse,
                    });
                }
            }
            reply(facts)
        },
    );
    *cancel.lock().unwrap() = Some(harness.service.clone());
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='failed' AND terminal_reason='user_cancel' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    // Every attempt of the round settled (none stays running), the
    // execution is cancelled, nothing was accepted or published.
    let deadline = Instant::now() + Duration::from_secs(20);
    let db = harness.db();
    loop {
        let running: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND a.state='running'",
                [&harness.pulse],
                |r| r.get(0),
            )
            .unwrap();
        let status: String = db
            .query_row(
                "SELECT status FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
                [&harness.pulse],
                |r| r.get(0),
            )
            .unwrap();
        if running == 0 && status == "cancelled" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "running={running} status={status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let accepted: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_acceptances a JOIN stage_executions e ON e.id=a.stage_execution_id WHERE e.pulsation_id=?1 AND e.stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(accepted, 0);
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
        ORIGINAL_SOURCE,
        "no publication after cancellation"
    );
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/session.rs")).unwrap(),
        SESSION_SOURCE
    );
}
