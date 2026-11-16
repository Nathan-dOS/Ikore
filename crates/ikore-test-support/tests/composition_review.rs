//! The Integrator's evidence boundary, review policy and convergence
//! (ADR-032) over the full pipeline v4: what the reviewer receives, what the
//! host records about it, the gaps that limit a verdict, the CLI scope and
//! the explicit decision a revision needs beyond the round threshold.
#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
mod composition_support;

use composition_support::*;
use ikore_core::orchestration::composition::review_boundary::CheckpointReviewInputs;
use ikore_core::orchestration::composition::settings::CompositionSettings;
use ikore_core::persistence_service::{
    ApplyHitlDecisionRequest, ConvergenceAck, HitlDecision, PersistenceService,
};
use ikore_test_support::IsolatedTestEnvironment;
use serde_json::Value;
use std::fs;
use std::sync::{Arc, Mutex, OnceLock};

static PATH_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const POLICY: &str =
    "# Review policy\n\nCheck expiry boundaries.\nOPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuv\n";

fn workspace_with_source() -> IsolatedTestEnvironment {
    let env = IsolatedTestEnvironment::new();
    fs::create_dir_all(env.workspace_dir().join("src")).unwrap();
    fs::write(env.workspace_dir().join("src/auth.rs"), ORIGINAL_SOURCE).unwrap();
    env
}

fn to_integrator(harness: &Harness) {
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Integrator");
}

fn review_manifest(harness: &Harness) -> (String, i64, Value) {
    let (state, gaps, manifest): (String, i64, String) = harness
        .db()
        .query_row(
            "SELECT r.review_policy_state,r.gap_count,r.manifest_json FROM integrator_review_inputs r JOIN stage_executions e ON e.id=r.stage_execution_id WHERE r.pulsation_id=?1 AND e.stage='Integrator' ORDER BY r.created_at DESC LIMIT 1",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    (state, gaps, serde_json::from_str(&manifest).unwrap())
}

fn candidate<'a>(manifest: &'a Value, path: &str) -> &'a Value {
    manifest["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["path"] == path)
        .unwrap_or_else(|| panic!("candidate `{path}` in {manifest}"))
}

fn request_revision(
    harness: &Harness,
    service: &PersistenceService,
    ack: Option<ConvergenceAck>,
) -> Result<(), String> {
    let checkpoint = service.get_hitl_checkpoint(&harness.pulse).unwrap();
    service
        .apply_hitl_decision_with_convergence(
            ApplyHitlDecisionRequest {
                pulsation_id: harness.pulse.clone(),
                checkpoint_id: checkpoint.checkpoint_id,
                expected_state_version: checkpoint.state_version,
                feedback: Some("Compare against the boundary instant.".into()),
                decision: HitlDecision::RequestRevision,
            },
            ack,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
fn the_integrator_reviews_classified_inputs_under_the_project_policy() {
    let env = workspace_with_source();
    fs::write(env.workspace_dir().join("REVIEW.md"), POLICY).unwrap();
    let seen: Arc<Mutex<Option<(String, String)>>> = Arc::default();
    let captured = seen.clone();
    let harness = Harness::start_in(env, vec![expiry_task()], move |facts, _, request| {
        if facts.executor == "integration_reviewer" {
            *captured.lock().unwrap() = Some((facts.user_prompt.clone(), request.body.clone()));
        }
        completed_reply(facts)
    });
    to_integrator(&harness);
    let (user, body) = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the reviewer was called");

    // The project policy arrives read-only, sanitized and below host gates.
    assert!(user.contains("## Review policy (read-only)"), "{user}");
    assert!(user.contains("Host gates and checks > approved intent"));
    assert!(user.contains("Check expiry boundaries."));
    assert!(
        !body.contains("sk-proj-abcdefghijklmnopqrstuv"),
        "the policy is sanitized before it leaves the host"
    );
    assert!(body.contains("REDACTED_SECRET"));
    // Base → candidate, over the sanitized views.
    assert!(user.contains("## Changes (base → candidate)"));
    assert!(user.contains("### `src/auth.rs` (modified)"), "{user}");
    assert!(user.contains("-    let _ = (expires_at, now);"));
    assert!(user.contains("+    expires_at > now"));
    assert!(user.contains("### `tests/auth_test.rs` (created)"));
    assert!(user.contains("## Candidate files (final, read-only)"));
    assert!(user.contains("## Host evidence"));
    // The author's narrative is labelled, and stage documents stay out.
    assert!(user.contains("## Author claims (narrative, not evidence)"));
    assert!(user.contains("A unit test exercises the criterion."));
    assert!(
        !user.contains("knowledge/obsidian/tests/"),
        "the Test report is not delivered"
    );
    assert!(
        !user.contains("knowledge/obsidian/features/"),
        "the Specification note is not delivered"
    );
    assert!(
        !user.contains("(owned: you may edit it)"),
        "the reviewer edits no candidate"
    );
    assert!(!user.contains("## Delivered content"));

    // The host records what the reviewer received.
    let (state, gaps, manifest) = review_manifest(&harness);
    assert_eq!(state, "loaded");
    // `src/auth.rs` is a sensitive path (ADR-034 M3): the only gaps are the
    // three check kinds a high level requires and no profile provides.
    assert_eq!(gaps, 3, "{manifest}");
    assert!(
        manifest["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gap| gap["code"] == "risk_check_missing"),
        "{manifest}"
    );
    assert_eq!(manifest["toolScope"], "host_context_only");
    assert_eq!(manifest["policy"]["files"][0]["path"], "REVIEW.md");
    assert!(
        !manifest.to_string().contains("sk-proj"),
        "no policy text is kept"
    );
    let source = candidate(&manifest, "src/auth.rs");
    assert_eq!(
        (&source["base"], &source["binding"], &source["diff"]),
        (
            &Value::from("present"),
            &Value::from("fresh"),
            &Value::from("included")
        )
    );
    let test = candidate(&manifest, "tests/auth_test.rs");
    assert_eq!(
        (&test["base"], &test["diff"]),
        (&Value::from("absent"), &Value::from("created"))
    );
    let documents = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| source["class"] == "stage_document")
        .collect::<Vec<_>>();
    assert!(!documents.is_empty(), "{manifest}");
    assert!(documents.iter().all(|source| source["delivered"] == false));
    let (base_state, base): (String, Option<String>) = harness
        .db()
        .query_row(
            "SELECT state,content FROM pulsation_file_bases WHERE pulsation_id=?1 AND path_key='src/auth.rs'",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (base_state.as_str(), base.as_deref()),
        ("present", Some(ORIGINAL_SOURCE))
    );

    // The checkpoint shows the record and notices a later policy edit.
    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    let changed = |service: &PersistenceService| match service
        .read_checkpoint_review_inputs(&checkpoint)
        .unwrap()
    {
        Some(CheckpointReviewInputs::Recorded {
            changed_policy_files,
            ..
        }) => changed_policy_files,
        other => panic!("recorded review inputs, got {other:?}"),
    };
    assert!(changed(&harness.service).is_empty());
    fs::write(
        harness.workspace().join("REVIEW.md"),
        "# Review policy\n\nStricter.\n",
    )
    .unwrap();
    assert_eq!(changed(&harness.service), ["REVIEW.md"]);

    // Base bytes serve the live review only.
    harness.approve();
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    let stored: i64 = harness
        .db()
        .query_row(
            "SELECT COUNT(content) FROM pulsation_file_bases WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, 0);
}

#[test]
fn a_hand_edit_after_publication_is_a_gap_that_downgrades_the_verdict() {
    let seen: Arc<Mutex<Option<String>>> = Arc::default();
    let captured = seen.clone();
    let harness = Harness::start(move |facts, _, _| {
        if facts.executor == "integration_reviewer" {
            *captured.lock().unwrap() = Some(facts.user_prompt.clone());
        }
        // The scripted reviewer claims `supported` on the edited file.
        completed_reply(facts)
    });
    for stage in ["SPECIFICATION", "Contract"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Test");
    fs::write(
        harness.workspace().join("src/auth.rs"),
        format!("// edited by hand\n{IMPLEMENTED_SOURCE}"),
    )
    .unwrap();
    harness.approve();
    harness.wait_for_checkpoint("Integrator");

    let user = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the reviewer was called");
    assert!(
        user.contains("`candidate_changed_outside_pipeline` (limits verdicts)"),
        "{user}"
    );
    let (_, gaps, manifest) = review_manifest(&harness);
    assert!(gaps >= 1);
    assert_eq!(
        candidate(&manifest, "src/auth.rs")["binding"],
        "changed_outside_pipeline"
    );
    let gap = manifest["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gap| gap["code"] == "candidate_changed_outside_pipeline")
        .unwrap();
    assert_eq!(gap["affectedCriteria"], serde_json::json!(["crit_1_1"]));
    let payload: String = harness
        .db()
        .query_row(
            "SELECT content FROM artifacts WHERE pulsation_id=?1 AND kind='stage_payload_v1' AND upper(producer_stage)='INTEGRATOR' ORDER BY created_at DESC LIMIT 1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert!(payload.contains(r#""verdict":"unverified""#), "{payload}");
    assert!(!payload.contains(r#""verdict":"supported""#), "{payload}");
}

#[test]
fn revision_rounds_beyond_the_threshold_need_an_explicit_decision() {
    let harness = Harness::start_with(
        workspace_with_source(),
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            revision_round_threshold: 1,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
    );
    to_integrator(&harness);
    let first = harness.service.convergence_status(&harness.pulse).unwrap();
    assert!(first.applies);
    assert_eq!((first.rounds, first.threshold), (0, 1));
    assert!(first.technical_attempts >= 6, "{first:?}");
    assert_eq!(first.candidate_generations, 1);
    request_revision(&harness, &harness.service, None).expect("the first round is free");

    // The revised Code runs again and Test is approved again.
    harness.wait_for_checkpoint("Test");
    harness.approve();
    harness.wait_for_checkpoint("Integrator");
    let error = request_revision(&harness, &harness.service, None).unwrap_err();
    assert!(error.contains("convergence_threshold_reached"), "{error}");
    let status = harness.service.convergence_status(&harness.pulse).unwrap();
    assert!(status.threshold_reached());
    assert_eq!(status.human_rounds, [("Integrator".to_owned(), 1)]);
    assert_eq!(status.candidate_generations, 2);
    let blank = request_revision(
        &harness,
        &harness.service,
        Some(ConvergenceAck::ContinueBeyondThreshold {
            reason: "   ".into(),
        }),
    )
    .unwrap_err();
    assert!(blank.contains("needs a reason"), "{blank}");
    request_revision(
        &harness,
        &harness.service,
        Some(ConvergenceAck::ContinueBeyondThreshold {
            reason: "The boundary case is still wrong.".into(),
        }),
    )
    .expect("an explicit decision admits the round");

    // A restart keeps the counts and the decision: a second service over
    // the same database, opened while the pipeline waits on the developer.
    harness.wait_for_checkpoint("Test");
    let reopened = PersistenceService::initialize(harness.env.data_dir()).unwrap();
    let status = reopened.convergence_status(&harness.pulse).unwrap();
    assert_eq!(status.rounds, 2);
    assert_eq!(status.decisions.len(), 1);
    assert_eq!(status.decisions[0].decision, "continue_beyond_threshold");
    assert_eq!(
        status.decisions[0].reason,
        "The boundary case is still wrong."
    );
    drop(reopened);

    harness.approve();
    harness.wait_for_checkpoint("Integrator");
    request_revision(&harness, &harness.service, None).expect_err("still beyond the threshold");
    request_revision(
        &harness,
        &harness.service,
        Some(ConvergenceAck::Replan {
            reason: "The approach needs a new specification.".into(),
        }),
    )
    .expect("replanning is a decision too");
    harness.wait_for_checkpoint("SPECIFICATION");
    let status = harness.service.convergence_status(&harness.pulse).unwrap();
    assert_eq!(status.rounds, 0, "a replan opens a new window: {status:?}");
    assert!(!status.threshold_reached());
    let events: i64 = harness
        .db()
        .query_row(
            "SELECT COUNT(*) FROM pulse_events WHERE pulsation_id=?1 AND event_type='convergence_decision'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 2);
}

/// Puts a `cli_fixture` copy on PATH under `name` while the guard lives.
struct FixtureOnPath {
    directory: tempfile::TempDir,
    original: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl FixtureOnPath {
    fn install(name: &str) -> Self {
        let lock = PATH_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_cli_fixture"),
            directory.path().join(format!("{name}.exe")),
        )
        .unwrap();
        let original = std::env::var_os("PATH");
        let mut paths = vec![directory.path().to_path_buf()];
        if let Some(original) = original.as_ref() {
            paths.extend(std::env::split_paths(original));
        }
        // SAFETY: this integration binary serializes its PATH mutations through PATH_TEST_LOCK,
        // held until `drop` restores the original value.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        Self {
            directory,
            original,
            _lock: lock,
        }
    }
}

impl Drop for FixtureOnPath {
    fn drop(&mut self) {
        match self.original.take() {
            // SAFETY: PATH_TEST_LOCK is still held by this guard.
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            // SAFETY: PATH_TEST_LOCK is still held by this guard.
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

#[test]
fn a_cli_integrator_reads_host_selected_context_only() {
    let fixture = FixtureOnPath::install("claude");
    let harness = Harness::start_prepared(
        workspace_with_source(),
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| {
            harness.route_role_to_official_cli("integrator", "claude");
        },
    );
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    // The official fixture does not answer with a review: the stage parks.
    // What matters is where and how the reviewer ran.
    harness.wait_for_failure_checkpoint();
    let scope = fs::read_to_string(fixture.directory.path().join("integrator.fixture-scope"))
        .expect("the reviewer ran through the CLI");
    assert!(scope.contains("project_files=false"), "{scope}");
    assert!(scope.contains("read_tools=false"), "{scope}");
    assert!(
        !harness
            .calls()
            .iter()
            .any(|call| call == "integration_reviewer"),
        "the reviewer never reached the API"
    );
}

#[test]
fn a_custom_cli_integrator_parks_before_any_invocation() {
    let harness = Harness::start_prepared(
        workspace_with_source(),
        vec![expiry_task()],
        CompositionSettings {
            enabled: true,
            ..CompositionSettings::default()
        }
        .with_all_recipes_qualified(),
        |facts, _, _| completed_reply(facts),
        |harness| harness.route_role_to_cli_bridge("integrator"),
    );
    for stage in ["SPECIFICATION", "Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    let checkpoint = harness.wait_for_failure_checkpoint();
    let db = harness.db();
    let (class, diagnostic): (String, String) = db
        .query_row(
            "SELECT failure_class,diagnostic FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&checkpoint],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(class, "provider_configuration");
    assert!(
        diagnostic.contains("official read-only CLI adapter"),
        "{diagnostic}"
    );
    assert!(
        !harness
            .calls()
            .iter()
            .any(|call| call == "integration_reviewer")
    );
    let runs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM terminal_runs WHERE pulsation_id=?1 AND agent_role='integrator'",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(runs, 0, "nothing was spawned");
}

fn approve_integrator(
    service: &PersistenceService,
    pulse: &str,
    reason: Option<&str>,
) -> Result<(), String> {
    let checkpoint = service.get_hitl_checkpoint(pulse).unwrap();
    service
        .apply_hitl_decision_with_acknowledgements(
            ApplyHitlDecisionRequest {
                pulsation_id: pulse.into(),
                checkpoint_id: checkpoint.checkpoint_id,
                expected_state_version: checkpoint.state_version,
                feedback: None,
                decision: HitlDecision::Approve,
            },
            None,
            reason.map(
                |reason| ikore_core::persistence_service::RiskAcknowledgement {
                    reason: reason.into(),
                },
            ),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// ADR-034 M3: the fixture edits `src/auth.rs`, a sensitive path, so the
/// review is high risk although the Intake said low. With no validation
/// profile, nothing backs the criterion but the model's reading, the
/// required check kinds are visible gaps that never change a verdict, and
/// approving needs an acknowledgement with a reason, recorded durably.
#[test]
fn a_high_risk_review_shows_its_coverage_and_needs_an_acknowledgement() {
    let seen: Arc<Mutex<Option<String>>> = Arc::default();
    let captured = seen.clone();
    let harness = Harness::start(move |facts, _, _| {
        if facts.executor == "integration_reviewer" {
            *captured.lock().unwrap() = Some(facts.user_prompt.clone());
        }
        completed_reply(facts)
    });
    to_integrator(&harness);
    let user = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the reviewer was called");
    assert!(user.contains("## Risk scrutiny"), "{user}");
    assert!(user.contains("**high** risk"));
    assert!(user.contains("sensitive path `src/auth.rs` (high)"));
    // The scrutiny section comes before the policy and the evidence.
    assert!(user.find("## Risk scrutiny") < user.find("## Host evidence"));

    let (_, _, manifest) = review_manifest(&harness);
    assert_eq!(manifest["schemaVersion"], 3);
    assert_eq!(manifest["risk"]["level"], "high");
    let missing = manifest["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|gap| gap["code"] == "risk_check_missing")
        .count();
    assert_eq!(missing, 3, "compile, test and lint: {manifest}");

    // The level was assessed at Code too, and the Integrator kept it.
    let levels: Vec<(String, String)> = harness
        .db()
        .prepare("SELECT stage,level FROM pulsation_risk_assessments WHERE pulsation_id=?1 ORDER BY stage")
        .unwrap()
        .query_map([&harness.pulse], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        levels,
        [
            ("Code".to_owned(), "high".to_owned()),
            ("Integrator".to_owned(), "high".to_owned()),
            ("Test".to_owned(), "high".to_owned()),
        ]
    );

    let status = harness
        .service
        .checkpoint_risk_status(&harness.pulse)
        .unwrap();
    assert!(status.applies);
    let matrix = status.matrix.as_ref().unwrap();
    assert!(
        matrix.rows.iter().all(|row| row.state
            != ikore_core::orchestration::composition::risk::CoverageState::Executed),
        "no check ran: {matrix:?}"
    );
    assert_eq!(matrix.missing_kinds, ["compile", "lint", "test"]);
    assert!(status.acknowledgement_required);

    // No reason, or a blank one: refused, and the review keeps waiting.
    let refused = approve_integrator(&harness.service, &harness.pulse, None).unwrap_err();
    assert!(
        refused.contains("risk_acknowledgement_required"),
        "{refused}"
    );
    let blank = approve_integrator(&harness.service, &harness.pulse, Some("   ")).unwrap_err();
    assert!(blank.contains("1 to 400"), "{blank}");
    let too_long = "x".repeat(401);
    assert!(approve_integrator(&harness.service, &harness.pulse, Some(&too_long)).is_err());
    assert!(harness.service.get_hitl_checkpoint(&harness.pulse).is_ok());

    approve_integrator(
        &harness.service,
        &harness.pulse,
        Some("The expiry check is covered by review; profiles come next."),
    )
    .unwrap();
    // A second service on the same database sees the same durable record.
    let restarted = PersistenceService::initialize(harness.env.data_dir()).unwrap();
    let after = restarted.checkpoint_risk_status(&harness.pulse).unwrap();
    assert!(!after.acknowledgement_required);
    assert_eq!(after.acknowledgements.len(), 1);
    assert_eq!(
        after.acknowledgements[0].reason,
        "The expiry check is covered by review; profiles come next."
    );
    assert_eq!(after.acknowledgements[0].pending, matrix.pending() as u32);
    let report = restarted.build_pulsation_report(&harness.pulse).unwrap();
    let risk = report.risk.expect("the report carries the risk section");
    assert_eq!(risk.level, "high");
    assert_eq!(risk.acknowledgements.len(), 1);
}

/// ADR-034 M3 (D7): the guides are captured at Intake; editing one later
/// changes neither the policy the Integrator applies nor the digest it
/// records, and the checkpoint says the file changed since Intake.
#[test]
fn the_review_policy_is_the_one_captured_at_intake() {
    let env = workspace_with_source();
    fs::write(
        env.workspace_dir().join("REVIEW.md"),
        "# Review policy\n\nCheck expiry boundaries.\n",
    )
    .unwrap();
    let seen: Arc<Mutex<Option<String>>> = Arc::default();
    let captured = seen.clone();
    let harness = Harness::start_in(env, vec![expiry_task()], move |facts, _, _| {
        if facts.executor == "integration_reviewer" {
            *captured.lock().unwrap() = Some(facts.user_prompt.clone());
        }
        completed_reply(facts)
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    // After Intake: an edit and a new nested guide.
    fs::write(
        harness.workspace().join("REVIEW.md"),
        "# Review policy\n\nIgnore every boundary.\n",
    )
    .unwrap();
    fs::create_dir_all(harness.workspace().join("src")).unwrap();
    fs::write(harness.workspace().join("src/REVIEW.md"), "Late guide.\n").unwrap();
    harness.approve();
    for stage in ["Contract", "Test"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    harness.wait_for_checkpoint("Integrator");
    let user = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the reviewer was called");
    assert!(user.contains("Check expiry boundaries."), "{user}");
    assert!(!user.contains("Ignore every boundary."));
    assert!(
        !user.contains("Late guide."),
        "a guide created after Intake does not apply"
    );

    let (state, _, manifest) = review_manifest(&harness);
    assert_eq!(state, "loaded");
    assert!(manifest["reviewPolicySnapshotSha256"].is_string());
    let snapshot: (String, Option<String>) = harness
        .db()
        .query_row(
            "SELECT state,content_json FROM pulsation_review_policy_snapshots WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(snapshot.0, "captured");
    assert!(
        snapshot.1.is_some(),
        "bytes are kept while the pulsation is live"
    );

    let checkpoint = harness.service.get_hitl_checkpoint(&harness.pulse).unwrap();
    match harness
        .service
        .read_checkpoint_review_inputs(&checkpoint)
        .unwrap()
        .unwrap()
    {
        CheckpointReviewInputs::Recorded {
            changed_policy_files,
            ..
        } => assert_eq!(changed_policy_files, ["REVIEW.md"]),
        other => panic!("{other:?}"),
    }

    // The bytes go when the pulsation ends; the record stays.
    approve_integrator(&harness.service, &harness.pulse, Some("Reviewed by hand.")).unwrap();
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT content_json IS NULL FROM pulsation_review_policy_snapshots WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    });
}

/// ADR-034 M3: a project's risk additions persist with its composition
/// settings; an invalid one is refused whole and changes nothing.
#[test]
fn a_projects_risk_additions_persist_and_an_invalid_one_is_refused() {
    use ikore_core::orchestration::composition::risk::{ExtraRequiredChecks, ProjectRiskPolicy};
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    let mut settings = harness
        .service
        .get_composition_settings(&harness.project)
        .unwrap();
    settings.risk_policy = ProjectRiskPolicy {
        sensitive_path_globs: vec!["billing/**".into()],
        extra_required_check_refs: ExtraRequiredChecks {
            medium: vec![ikore_core::orchestration::composition::Handle::new("audit").unwrap()],
            high: Vec::new(),
        },
    };
    harness
        .service
        .update_composition_settings(&harness.project, settings.clone())
        .unwrap();
    assert_eq!(
        harness
            .service
            .get_composition_settings(&harness.project)
            .unwrap()
            .risk_policy,
        settings.risk_policy
    );
    let mut invalid = settings.clone();
    invalid.risk_policy.sensitive_path_globs = vec!["src/[".into()];
    assert!(
        harness
            .service
            .update_composition_settings(&harness.project, invalid)
            .is_err()
    );
    assert_eq!(
        harness
            .service
            .get_composition_settings(&harness.project)
            .unwrap()
            .risk_policy,
        settings.risk_policy
    );
}
