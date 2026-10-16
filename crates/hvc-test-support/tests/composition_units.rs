//! Pipeline v4 with several cohesive units per stage (ADR-022, M3): units
//! planned from the approved proposals, dependency order with candidate
//! delivery, staging outside the workspace, ownership enforcement and reuse
//! of validated siblings across a Retry generation (SC-004, SC-006, SC-009,
//! SC-016).
mod composition_support;

use composition_support::*;
use hvc_core::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind};
use hvc_core::persistence_service::{ResolveStageFailureRequest, StageFailureDecision};
use hvc_test_support::{IsolatedTestEnvironment, ScriptedResponse};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const SESSION_SOURCE: &str = "pub fn open_session(expires_at: u64, now: u64) -> bool {\n    let _ = (expires_at, now);\n    true\n}\n";
const SESSION_IMPLEMENTED: &str = "pub fn open_session(expires_at: u64, now: u64) -> bool {\n    crate::auth::validate_token(expires_at, now)\n}\n";

fn session_task() -> TaskDraft {
    TaskDraft {
        title: "Sessions reject expired tokens".into(),
        kind: TaskKind::Bug,
        description:
            "open_session in src/session.rs must use validate_token; add tests/session_test.rs"
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

/// Two code units (the session one reads the validator's output) and two
/// test units, one per task.
fn proposals(facts: &PromptFacts) -> Value {
    let auth = facts.handle_for_path("src/auth.rs");
    let session = facts.handle_for_path("src/session.rs");
    let auth_test = facts.handle_for_path("tests/auth_test.rs");
    let session_test = facts.handle_for_path("tests/session_test.rs");
    json!([
        {"unitKey": "implement_validator", "kind": "code", "taskRefs": ["task_1"], "criterionRefs": ["crit_1_1"],
         "inputRefs": [auth], "destinationRefs": [auth], "dependsOn": [], "objective": "Compare the expiry against the clock."},
        {"unitKey": "implement_session", "kind": "code", "taskRefs": ["task_2"], "criterionRefs": ["crit_2_1"],
         "inputRefs": [auth, session], "destinationRefs": [session], "dependsOn": [], "objective": "Route open_session through the validator."},
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

fn staged_auth(data_dir: &Path) -> Option<String> {
    let root = data_dir.join("stage_candidates");
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path().join("reconstructed/src/auth.rs");
        if let Ok(content) = fs::read_to_string(&path) {
            return Some(content);
        }
    }
    None
}

/// What the session unit's worker could observe while it was being asked.
#[derive(Default, Debug)]
struct SessionCallObservation {
    candidate_paths: Vec<String>,
    prompt_has_dependency_output: bool,
    workspace_auth_at_call: Option<String>,
    staged_auth_at_call: Option<String>,
}

#[test]
fn dependent_code_units_run_in_order_with_candidates_and_staging_off_the_workspace() {
    let env = fixture_env();
    let workspace: PathBuf = env.workspace_dir().to_path_buf();
    let data_dir: PathBuf = env.data_dir().to_path_buf();
    let observed: Arc<Mutex<SessionCallObservation>> = Arc::default();
    let sink = observed.clone();
    let harness = Harness::start_in(
        env,
        vec![expiry_task(), session_task()],
        move |facts, _, _| {
            if facts.executor == "code_implementer" && !facts.candidates().is_empty() {
                let mut observation = sink.lock().unwrap();
                observation.candidate_paths = facts
                    .candidates()
                    .iter()
                    .map(|(_, row)| row.path.clone())
                    .collect();
                observation.prompt_has_dependency_output =
                    facts.user_prompt.contains("## Dependency outputs")
                        && facts.user_prompt.contains(IMPLEMENTED_SOURCE);
                observation.workspace_auth_at_call =
                    fs::read_to_string(workspace.join("src/auth.rs")).ok();
                observation.staged_auth_at_call = staged_auth(&data_dir);
            }
            reply(facts)
        },
    );
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
            "code_implementer",
            "test_author",
            "test_author",
            "integration_reviewer",
        ],
        "two code units and two test units, one call each"
    );

    let observation = observed.lock().unwrap();
    assert_eq!(
        observation.candidate_paths,
        vec!["src/auth.rs"],
        "the session unit reads the validator's candidate"
    );
    assert!(
        observation.prompt_has_dependency_output,
        "the candidate bytes are delivered, not the baseline"
    );
    assert_eq!(
        observation.workspace_auth_at_call.as_deref(),
        Some(ORIGINAL_SOURCE),
        "no live file is written on child success (SC-009)"
    );
    assert_eq!(
        observation.staged_auth_at_call.as_deref(),
        Some(IMPLEMENTED_SOURCE),
        "the validated output is staged outside the workspace"
    );

    let db = harness.db();
    let code_units: Vec<(String, i64, Option<String>)> = db
        .prepare("SELECT u.logical_unit_key,u.ordinal,(SELECT GROUP_CONCAT(p.logical_unit_key) FROM stage_unit_dependencies d JOIN stage_work_units p ON p.id=d.predecessor_unit_id WHERE d.successor_unit_id=u.id) FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' ORDER BY u.ordinal")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        code_units,
        vec![
            ("implement_validator".into(), 0, None),
            (
                "implement_session".into(),
                1,
                Some("implement_validator".into())
            ),
        ]
    );
    let plan: String = db
        .query_row(
            "SELECT plan_json FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    let plan: Value = serde_json::from_str(&plan).unwrap();
    let candidate = plan["references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "candidate_source")
        .expect("the frozen plan carries the candidate reference");
    assert_eq!(candidate["relativePath"], "src/auth.rs");
    assert_eq!(candidate["producerUnitId"], plan["units"][0]["unitId"]);
    let anchors = plan["anchors"].as_array().unwrap();
    assert!(!anchors.is_empty(), "delivered sources carry exact anchors");

    // Both files reached the workspace only through acceptance and the outbox.
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE
    );
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/session.rs")).unwrap(),
        SESSION_IMPLEMENTED
    );
    assert!(harness.workspace().join("tests/auth_test.rs").exists());
    assert!(harness.workspace().join("tests/session_test.rs").exists());
    let evidence: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM task_evidence WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, 2, "one evidence row per task in the aggregate");
}

#[test]
fn an_edit_outside_ownership_is_rejected_and_corrected_once() {
    let env = fixture_env();
    let harness = Harness::start_in(
        env,
        vec![expiry_task(), session_task()],
        move |facts, _, request| {
            if facts.executor == "code_implementer"
                && let Some((candidate, _)) = facts.candidates().first()
                && !request.body.contains("## Correction")
            {
                // Writes the candidate it may only read.
                let task = facts.tasks[0].as_str();
                return provider_reply(json!({
                    "schemaVersion": 1, "outcome": "completed", "executorKind": "code_implementer",
                    "payload": {
                        "edits": [{"artifactKey": "edit_1", "edit": {"operation": "replace_file", "sourceRef": candidate, "content": "pub fn hijacked() {}\n"}}],
                        "evidence": [{"taskRef": task, "criterionRefs": facts.criteria_of(task), "artifactKeys": ["edit_1"], "inputRefs": [candidate], "explanation": "Rewrote the validator instead."}],
                        "usedRefs": [candidate]
                    }
                }));
            }
            if facts.executor == "code_repairer" {
                // The repair replaces the rejected answer whole.
                assert!(
                    facts
                        .user_prompt
                        .contains("return the COMPLETE response again")
                );
                assert!(
                    facts
                        .user_prompt
                        .contains("- Deliver the complete edits for: `src/session.rs`.")
                );
            }
            reply(facts)
        },
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_checkpoint("Test");
    // The correction runs the unit's repair specialty, once, then the stage
    // completes and Test proceeds.
    assert_eq!(
        &harness.calls()[..6],
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "code_implementer",
            "code_repairer",
        ]
    );
    let db = harness.db();
    let (code, diagnostic): (Option<String>, Option<String>) = db
        .query_row(
            "SELECT a.error_code,a.diagnostic FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND a.state='rejected'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(code.as_deref(), Some("ownership_violation"));
    assert!(
        diagnostic.unwrap().contains("cand_1"),
        "the diagnostic names the handle the unit may only read"
    );
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE,
        "the validator's own output was published, not the hijack"
    );
}

#[test]
fn a_validated_sibling_is_reused_without_a_call_when_the_stage_is_retried() {
    let env = fixture_env();
    let harness = Harness::start_in(
        env,
        vec![expiry_task(), session_task()],
        move |facts, ordinal, _| {
            // The session unit's first call fails with a provider configuration
            // fault (nothing a resample fixes, so the stage parks at once); the
            // validator unit succeeded before and must not be paid again.
            if facts.executor == "code_implementer"
                && !facts.candidates().is_empty()
                && ordinal == 5
            {
                return ScriptedResponse::json(
                    401,
                    r#"{"error":{"message":"fixture key rejected"}}"#,
                );
            }
            reply(facts)
        },
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    let checkpoint = harness.wait_for_failure_checkpoint();
    assert_eq!(harness.calls().len(), 5, "validator once, session once");
    let db = harness.db();
    let parked: (String, String) = db
        .prepare("SELECT u.logical_unit_key,u.state FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' AND e.status='parked' AND u.logical_unit_key='implement_validator'")
        .unwrap()
        .query_row([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(parked, ("implement_validator".into(), "validated".into()));

    harness
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: checkpoint,
            expected_state_version: harness.state_version(),
            decision: StageFailureDecision::RetryStage,
            feedback: None,
        })
        .unwrap();
    harness.wait_for_checkpoint("Test");
    let calls = harness.calls();
    assert_eq!(calls.len(), 8, "{calls:?}");
    assert_eq!(
        &calls[5..],
        ["code_implementer", "test_author", "test_author"],
        "only the session unit ran again"
    );

    let units: Vec<(String, String, Option<String>, i64)> = db
        .prepare("SELECT u.logical_unit_key,u.state,u.reused_from_attempt_id,(SELECT COUNT(*) FROM stage_invocation_reservations r WHERE r.unit_id=u.id) FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' AND e.status='accepted' ORDER BY u.ordinal")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(units.len(), 2, "{units:?}");
    assert_eq!(units[0].0, "implement_validator");
    assert_eq!(units[0].1, "validated");
    assert!(units[0].2.is_some(), "reused from its original attempt");
    assert_eq!(
        units[0].3, 0,
        "a reused result holds no permit and creates no usage"
    );
    assert_eq!(units[1].0, "implement_session");
    assert!(units[1].2.is_none());
    assert_eq!(units[1].3, 1);
    let pools: Vec<String> = db
        .prepare("SELECT r.pool FROM stage_invocation_reservations r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' ORDER BY r.created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        pools,
        ["initial", "initial", "extra"],
        "the retry generation spends the extra permit, never a renewed budget"
    );
    let executions: Vec<(String, Option<String>)> = db
        .prepare("SELECT status,predecessor_execution_id FROM stage_executions WHERE pulsation_id=?1 AND stage='Code' ORDER BY created_at")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(executions.len(), 2);
    assert_eq!(executions[0].0, "superseded");
    assert_eq!(executions[1].0, "accepted");
    assert!(
        executions[1].1.is_some(),
        "the retry execution links its parked predecessor"
    );
    let accounts: i64 = db
        .query_row(
            "SELECT COUNT(DISTINCT budget_account_id) FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(accounts, 1, "Retry shares the stage's budget account");
}

/// The session task with a second criterion, so one of its criteria can be
/// shared between the two code units while each keeps one of its own.
fn session_task_with_logging() -> TaskDraft {
    let mut task = session_task();
    task.criteria.push(AcceptanceCriterion {
        id: "ac-3".into(),
        description: "open_session logs the rejected expiry".into(),
    });
    task
}

/// Like `proposals`, but the validator unit also carries the session
/// criterion `shared`, which the session unit carries too: a criterion
/// shared by two code units, as the Specification assigned them in "Teste
/// Haiku Only 1" (2026-09-24).
fn shared_proposals(facts: &PromptFacts, session_criteria: &[&str], shared: &str) -> Value {
    let mut proposals = proposals(facts);
    proposals[0]["taskRefs"] = json!(["task_1", "task_2"]);
    proposals[0]["criterionRefs"] = json!(["crit_1_1", shared]);
    proposals[1]["criterionRefs"] = json!(session_criteria);
    proposals[3]["criterionRefs"] = json!(session_criteria);
    proposals
}

/// A reply whose code evidence leaves out `dropped` for every unit that owns
/// one of `skippers`.
fn shared_reply(
    facts: &PromptFacts,
    session_criteria: &[&str],
    shared: &str,
    skippers: &[&str],
    dropped: &str,
) -> ScriptedResponse {
    let mut options = options();
    if facts.executor == "specification_author" {
        options.proposals = Some(shared_proposals(facts, session_criteria, shared));
    }
    let mut reply = unit_response_with(facts, &options);
    let skips = facts.executor == "code_implementer"
        && facts
            .owned()
            .iter()
            .any(|(_, row)| skippers.contains(&row.path.as_str()));
    if skips {
        let evidence = reply["payload"]["evidence"].as_array_mut().unwrap();
        for item in evidence.iter_mut() {
            let kept = item["criterionRefs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|criterion| *criterion != dropped)
                .cloned()
                .collect::<Vec<_>>();
            item["criterionRefs"] = Value::Array(kept);
        }
        evidence.retain(|item| !item["criterionRefs"].as_array().unwrap().is_empty());
    }
    provider_reply(reply)
}

#[test]
fn a_unit_may_leave_a_shared_criterion_to_the_sibling_that_delivers_it() {
    // The validator carries the session criterion but only the session unit
    // evidences it.
    let harness = Harness::start_in(
        fixture_env(),
        vec![expiry_task(), session_task()],
        |facts, _, _| {
            shared_reply(
                facts,
                &["crit_2_1"],
                "crit_2_1",
                &["src/auth.rs"],
                "crit_2_1",
            )
        },
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_checkpoint("Test");
    assert_eq!(
        &harness.calls()[3..5],
        ["code_implementer", "code_implementer"],
        "no correction: the validator owes only its exclusive criterion"
    );
    let rejected: i64 = harness
        .db()
        .query_row(
            "SELECT COUNT(*) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND a.state='rejected'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rejected, 0);
}

#[test]
fn a_shared_criterion_no_unit_evidenced_parks_the_stage_once() {
    // `crit_2_2` is shared; each unit evidences only its own criterion.
    let harness = Harness::start_in(
        fixture_env(),
        vec![expiry_task(), session_task_with_logging()],
        |facts, _, _| {
            shared_reply(
                facts,
                &["crit_2_1", "crit_2_2"],
                "crit_2_2",
                &["src/auth.rs", "src/session.rs"],
                "crit_2_2",
            )
        },
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    let checkpoint = harness.wait_for_failure_checkpoint();
    assert_eq!(
        harness.calls().len(),
        5,
        "both units validated; nothing a resample could fix is re-asked"
    );
    let (class, code, diagnostic, targets): (String, String, String, String) = harness
        .db()
        .query_row(
            "SELECT failure_class,error_code,diagnostic,allowed_revision_targets_json FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(class, "host_contract");
    assert_eq!(code, "evidence_unresolved");
    assert!(
        diagnostic.contains("`crit_2_2` (units `implement_validator`, `implement_session`)"),
        "{diagnostic}"
    );
    assert_eq!(targets, r#"["Code"]"#);
}

#[test]
fn an_unqualified_recipe_keeps_one_unit_even_when_the_specification_proposes_several() {
    // Composition on, no recipe promoted (ADR-022 M7 qualification gate):
    // the proposals are recorded, but Code runs the single `implement_batch`
    // unit - the benchmark's arm B.
    let harness = Harness::start_with(
        fixture_env(),
        vec![expiry_task(), session_task()],
        hvc_core::orchestration::composition::settings::CompositionSettings {
            enabled: true,
            ..Default::default()
        },
        |facts, _, _| reply(facts),
    );
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_checkpoint("Test");
    let units: Vec<String> = harness
        .db()
        .prepare("SELECT u.logical_unit_key FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id WHERE e.pulsation_id=?1 AND e.stage='Code' ORDER BY u.ordinal")
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(units, ["implement_batch"], "{units:?}");
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|c| c.as_str() == "code_implementer")
            .count(),
        1
    );
}
