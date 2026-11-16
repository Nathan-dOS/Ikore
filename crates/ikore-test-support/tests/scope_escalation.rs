//! ADR-035 M4 over the real pipeline v4: a Code unit that must change a
//! delivered file it does not own asks for it, the developer grants it at the
//! failure checkpoint, and the next generation owns and publishes it. Also:
//! a file changed after the request, a Test unit asking for a production
//! file, a restart between the grant and the dispatch, and a revision that
//! supersedes a granted request.
#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
mod composition_support;

use composition_support::*;
use ikore_core::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind};
use ikore_core::orchestration::mandate::{
    MandateCause, SCOPE_GRANT_COMMAND_VERSION, ScopeGrantCommandV1, WriteClass,
};
use ikore_core::orchestration::review_brief::{FailureDecisionKind, failure_choices};
use ikore_core::persistence_service::*;
use ikore_test_support::pulsation_analysis::{Selector, analyze, render_text};
use ikore_test_support::{IsolatedTestEnvironment, RecordedRequest, ScriptedResponse};
use serde_json::{Value, json};
use std::fs;

const UTIL_SOURCE: &str = "pub fn now() -> u64 {\n    0\n}\n";
const UTIL_IMPLEMENTED: &str = "pub fn now() -> u64 {\n    std::time::SystemTime::now()\n        .duration_since(std::time::UNIX_EPOCH)\n        .map_or(0, |elapsed| elapsed.as_secs())\n}\n";

fn scope_task() -> TaskDraft {
    TaskDraft {
        title: "Reject expired tokens".into(),
        kind: TaskKind::Bug,
        description: "validate_token in src/auth.rs accepts every token; compare against the clock in src/util.rs and add a unit test in tests/auth_test.rs".into(),
        criteria: vec![AcceptanceCriterion {
            id: "ac-1".into(),
            description: "An expired token is rejected by validate_token".into(),
        }],
        dependencies: vec![],
        feature_id: None,
    }
}

fn fixture_env() -> IsolatedTestEnvironment {
    let env = IsolatedTestEnvironment::new();
    fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    fs::write(env.workspace_dir().join("src/util.rs"), UTIL_SOURCE).unwrap();
    env
}

/// One Code unit that writes the validator and only reads the clock helper.
fn proposals(facts: &PromptFacts) -> Value {
    let auth = facts.handle_for_path("src/auth.rs");
    let util = facts.handle_for_path("src/util.rs");
    json!([{
        "unitKey": "implement_expiry", "kind": "code", "taskRefs": ["task_1"], "criterionRefs": ["crit_1_1"],
        "inputRefs": [auth, util], "destinationRefs": [auth], "dependsOn": [], "objective": "Compare the expiry against the clock."
    }])
}

/// Every specialty answers as usual, except that a Code or Test worker
/// listed in `asks` fails with `scope_insufficient` naming those files while
/// it reads them without owning them.
fn reply(facts: &PromptFacts, asks: &[(&str, &str)]) -> ScriptedResponse {
    let mut options = ReplyOptions::default();
    options
        .contents
        .insert("src/util.rs".into(), UTIL_IMPLEMENTED.into());
    if facts.executor == "specification_author" {
        options.proposals = Some(proposals(facts));
    }
    let missing = asks
        .iter()
        .filter(|(executor, _)| facts.executor == *executor)
        .filter(|(_, path)| {
            facts
                .references
                .values()
                .any(|row| row.path == *path && !row.owned && !row.candidate && !row.planned)
        })
        .map(|(_, path)| facts.handle_for_path(path).to_owned())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return provider_reply(json!({
            "schemaVersion": 1, "outcome": "failed", "code": "scope_insufficient",
            "message": "The clock helper this unit reads must change too.",
            "relatedRefs": missing
        }));
    }
    provider_reply(unit_response_with(facts, &options))
}

fn code_asks_for_util(facts: &PromptFacts, _: usize, _: &RecordedRequest) -> ScriptedResponse {
    reply(facts, &[("code_implementer", "src/util.rs")])
}

fn start() -> Harness {
    Harness::start_in(fixture_env(), vec![scope_task()], code_asks_for_util)
}

fn failure(service: &PersistenceService, project: &str) -> StageFailureCheckpoint {
    service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: project.into(),
        })
        .unwrap()
        .snapshot
        .failure_checkpoints
        .into_iter()
        .next()
        .expect("a waiting failure checkpoint")
}

fn grant(
    service: &PersistenceService,
    failure: &StageFailureCheckpoint,
) -> Result<ResolveStageFailureResponse, String> {
    let request = failure.scope_request.as_ref().unwrap();
    service
        .grant_scope_and_retry(ScopeGrantCommandV1 {
            schema_version: SCOPE_GRANT_COMMAND_VERSION,
            project_id: failure.project_id.clone(),
            pulsation_id: failure.pulsation_id.clone(),
            checkpoint_id: failure.checkpoint_id.clone(),
            request_id: request.request_id.clone(),
            expected_state_version: failure.state_version,
            expected_derivation_sha256: request.derivation_sha256.clone(),
        })
        .map_err(|error| error.to_string())
}

fn request_states(harness: &Harness) -> Vec<String> {
    harness
        .db()
        .prepare("SELECT state FROM mandate_scope_requests WHERE pulsation_id=?1 ORDER BY created_at,rowid")
        .unwrap()
        .query_map([&harness.pulse], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn calls(harness: &Harness, executor: &str) -> usize {
    harness
        .calls()
        .iter()
        .filter(|call| call.as_str() == executor)
        .count()
}

fn wait_done(harness: &Harness) {
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
}

/// From the parked Code stage to the end: the request, the grant, the new
/// mandate revision and receipt, the shared budget and the publication.
#[test]
fn a_granted_file_becomes_the_units_own_and_is_published() {
    let harness = start();
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    let checkpoint = harness.wait_for_failure_checkpoint();
    let parked = failure(&harness.service, &harness.project);
    assert_eq!(parked.checkpoint_id, checkpoint);
    assert_eq!(parked.stage, PulsationStage::Code);
    assert_eq!(parked.failure_class, "model_reported_failure");
    assert!(
        !parked.allow_retry,
        "a plain Retry would ask the same again"
    );
    let request = parked.scope_request.clone().expect("a scope request");
    assert_eq!(request.state, "pending");
    assert_eq!(request.unit_key, "implement_expiry");
    assert_eq!(request.stage, PulsationStage::Code);
    assert_eq!(
        request
            .accepted
            .iter()
            .map(|path| path.path.as_str())
            .collect::<Vec<_>>(),
        ["src/util.rs"]
    );
    assert_eq!(
        request.accepted[0].sha256,
        ikore_core::orchestration::ContentSha256::of(UTIL_SOURCE.as_bytes()).as_str()
    );
    assert!(
        request
            .agent_message
            .as_deref()
            .is_some_and(|message| message.contains("clock helper"))
    );
    assert!(request.remaining_extra_invocations >= 1);
    let choices = failure_choices(&parked);
    assert_eq!(choices[0].decision, FailureDecisionKind::GrantScope);
    assert!(choices[0].effect.contains("`implement_expiry`"));

    grant(&harness.service, &parked).unwrap();
    let mandates = harness.service.pulsation_mandates(&harness.pulse).unwrap();
    assert_eq!(
        mandates
            .iter()
            .map(|record| (record.revision, record.cause))
            .collect::<Vec<_>>(),
        [(1, MandateCause::Issued), (2, MandateCause::ScopeGrant)]
    );
    let amendment = mandates[1]
        .mandate
        .scope
        .grants
        .iter()
        .find(|grant| grant.path == "src/util.rs")
        .unwrap();
    assert!(amendment.origins.contains(&WriteClass::Amendment));
    assert_eq!(amendment.unit_key.as_deref(), Some("implement_expiry"));
    assert_eq!(amendment.amendment_revision, Some(2));
    let receipt = harness
        .service
        .checkpoint_receipt(&checkpoint)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.decision, "grant_scope");
    assert_eq!(receipt.confirmed_paths, ["src/util.rs"]);
    assert_eq!(receipt.mandate_revision, Some(2));
    assert_eq!(request_states(&harness), ["granted"]);
    assert!(
        grant(&harness.service, &parked).is_err(),
        "a request is granted once"
    );

    harness.wait_for_checkpoint("Test");
    harness.approve();
    harness.wait_for_checkpoint("Integrator");
    harness.approve();
    wait_done(&harness);
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/util.rs")).unwrap(),
        UTIL_IMPLEMENTED
    );
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE
    );
    assert_eq!(
        calls(&harness, "code_implementer"),
        2,
        "one call that asked, one that wrote"
    );
    let db = harness.db();
    let (executions, accounts): (u32, u32) = db
        .query_row(
            "SELECT COUNT(*),COUNT(DISTINCT budget_account_id) FROM stage_executions WHERE pulsation_id=?1 AND stage='Code'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (executions, accounts),
        (2, 1),
        "the retry shares the budget"
    );
    let extra: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_invocation_reservations r JOIN stage_executions e ON e.budget_account_id=r.budget_account_id WHERE e.pulsation_id=?1 AND e.stage='Code' AND r.pool='extra' AND r.state<>'released'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(extra >= 1, "the retry drew on the extra pool");
    let events: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM mandate_enforcement_events WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 0, "the granted write stays within the mandate");

    // The post-run analysis reads the same story back.
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attestation_seal_intents WHERE pulsation_id=?1 AND state<>'pending')",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let report = analyze(
        harness.service.database_path(),
        &Selector::Id(harness.pulse.clone()),
    )
    .unwrap();
    let governance = &report["governance"];
    assert_eq!(
        governance["mandates"]["revisions_by_cause"]["scope_grant"],
        1
    );
    assert_eq!(governance["mandates"]["enforcement"], "enforced_v4");
    assert_eq!(governance["mandates"]["refused_writes"], 0);
    assert_eq!(governance["scope_requests"]["total"], 1);
    assert_eq!(governance["scope_requests"]["by_state"]["granted"], 1);
    assert_eq!(governance["scope_requests"]["grant_percent"], 100.0);
    assert_eq!(governance["scope_requests"]["files_granted"], 1);
    assert!(
        governance["scope_requests"]["parked_seconds"]
            .as_f64()
            .unwrap()
            >= 0.0
    );
    assert_eq!(governance["attestation"]["intent"], "sealed");
    assert_eq!(governance["attestation"]["signature"], "signed");
    assert_eq!(governance["attestation"]["outcome"], "done");
    assert!(
        governance["attestation"]["statement_bytes"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(
        governance["attestation"]["seal_latency_seconds"]
            .as_f64()
            .unwrap()
            >= 0.0
    );
    let text = render_text(&report);
    assert!(text.contains("== Governance =="), "{text}");
    assert!(text.contains("scope_grant=1"), "{text}");
    assert!(
        !text.contains("WARNING: the mandate refused writes"),
        "{text}"
    );
}

#[test]
fn a_file_changed_after_the_request_is_not_granted() {
    let harness = start();
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    let parked = failure(&harness.service, &harness.project);
    fs::write(
        harness.workspace().join("src/util.rs"),
        "pub fn now() -> u64 {\n    1\n}\n",
    )
    .unwrap();
    let refused = grant(&harness.service, &parked).unwrap_err().to_string();
    assert!(
        refused.contains("`src/util.rs` changed since the request"),
        "{refused}"
    );
    assert_eq!(request_states(&harness), ["stale"]);
    assert_eq!(
        harness
            .service
            .pulsation_mandates(&harness.pulse)
            .unwrap()
            .len(),
        1,
        "nothing was granted"
    );
    let still = failure(&harness.service, &harness.project);
    assert_eq!(still.checkpoint_id, parked.checkpoint_id);
    assert_eq!(still.scope_request.as_ref().unwrap().state, "stale");
    assert!(
        failure_choices(&still)
            .iter()
            .all(|choice| choice.decision != FailureDecisionKind::GrantScope)
    );
    harness
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: still.checkpoint_id,
            expected_state_version: still.state_version,
            decision: StageFailureDecision::Abort,
            feedback: None,
        })
        .unwrap();
    assert_eq!(
        request_states(&harness),
        ["stale"],
        "a stale request stays stale"
    );
}

#[test]
fn another_decision_declines_the_request_with_its_action() {
    let harness = start();
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    let parked = failure(&harness.service, &harness.project);
    harness
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: parked.checkpoint_id.clone(),
            expected_state_version: parked.state_version,
            decision: StageFailureDecision::Abort,
            feedback: None,
        })
        .unwrap();
    let (state, decision): (String, String) = harness
        .db()
        .query_row(
            "SELECT state,decision FROM mandate_scope_requests WHERE checkpoint_id=?1",
            [&parked.checkpoint_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), decision.as_str()), ("declined", "abort"));
    assert!(
        grant(&harness.service, &parked).is_err(),
        "a declined request is never granted"
    );
}

#[test]
fn a_test_unit_asking_for_a_production_file_gets_no_request() {
    let harness = Harness::start_in(fixture_env(), vec![scope_task()], |facts, _, _| {
        reply(facts, &[("test_author", "src/auth.rs")])
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    let parked = failure(&harness.service, &harness.project);
    assert_eq!(parked.stage, PulsationStage::Test);
    assert_eq!(parked.failure_class, "model_reported_failure");
    assert!(parked.scope_request.is_none());
    assert!(request_states(&harness).is_empty());
}

/// The grant commits the retry with the mandate; an application closed
/// before the worker dispatched it resumes it on the next start.
#[test]
fn a_restart_between_the_grant_and_the_dispatch_resumes_the_stage() {
    let harness = start();
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    let stopped = harness.stop();
    let parked = failure(&stopped.service, &stopped.project);
    grant(&stopped.service, &parked).unwrap();
    let (status, generation): (String, u32) = rusqlite::Connection::open(
        stopped.service.database_path(),
    )
    .unwrap()
    .query_row(
        "SELECT status,generation FROM jobs WHERE pulsation_id=?1 AND kind='run_stage::Code'",
        [&stopped.pulse],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap();
    assert_eq!((status.as_str(), generation), ("pending", 2));
    let harness = stopped.restart(code_asks_for_util);
    harness.wait_for_checkpoint("Test");
    harness.approve();
    harness.wait_for_checkpoint("Integrator");
    harness.approve();
    wait_done(&harness);
    assert_eq!(
        fs::read_to_string(harness.workspace().join("src/util.rs")).unwrap(),
        UTIL_IMPLEMENTED
    );
}

/// A revision of the stage supersedes the grant: the next plan no longer
/// gives the unit the file, so it asks again under a new budget.
#[test]
fn a_revision_after_the_grant_supersedes_the_request() {
    let harness = start();
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    harness.wait_for_failure_checkpoint();
    grant(
        &harness.service,
        &failure(&harness.service, &harness.project),
    )
    .unwrap();
    harness.wait_for_checkpoint("Test");
    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    harness
        .service
        .apply_hitl_decision(ApplyHitlDecisionRequest {
            pulsation_id: harness.pulse.clone(),
            checkpoint_id: checkpoint.checkpoint_id,
            expected_state_version: checkpoint.state_version,
            feedback: Some("Keep the helper as it was.".into()),
            decision: HitlDecision::RequestRevision,
        })
        .unwrap();
    harness.wait_for_failure_checkpoint();
    let again = failure(&harness.service, &harness.project);
    assert_eq!(again.stage, PulsationStage::Code);
    let second = again.scope_request.expect("the unit asks again");
    assert_eq!(second.state, "pending");
    assert_eq!(request_states(&harness), ["superseded", "pending"]);
    assert_eq!(
        harness
            .service
            .pulsation_mandates(&harness.pulse)
            .unwrap()
            .len(),
        2,
        "the mandate keeps the grant as history; only a new Specification approval re-issues it"
    );
}
