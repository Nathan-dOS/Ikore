#![allow(
    clippy::unwrap_used,
    reason = "Test-only harnesses fail immediately when fixture setup is invalid"
)]
//! Development fixture for ADR-027: a v4 pulsation parked at its Specification
//! checkpoint, so the real GUI can be opened on the Review Brief tab.
//!
//! Usage: `review_brief_fixture NEW_DIRECTORY [--revised] [--stage STAGE]`,
//! then start `hvc-app` with `HVC_DATA_DIR` set to the printed data
//! directory. Every provider reply comes from the scripted composition
//! worker; the worker stops once the checkpoint is waiting, so the GUI never
//! races it. Provider credentials point at fixture-only names, never the
//! developer's.
//!
//! `--revised` first requests a revision with feedback composed from flags,
//! as Review sends it, and parks at the second Specification checkpoint, whose
//! brief shows what changed (ADR-027 M3).
//!
//! `--stage contract|test|integrator` approves on to that checkpoint;
//! `--stage failure` makes the code worker diagnose a contract conflict, which
//! parks a stage failure with three revision targets (ADR-027 M4). When the
//! `check_fixture` binary sits next to the examples folder, a required compile
//! check runs, so the Test brief carries a host receipt.
//!
//! `--visualizer` enables the Visualizer (ADR-027 M5) and waits for the note
//! of the parked checkpoint before the worker stops.
//!
//! `--review-policy` writes a project `REVIEW.md` first, so the Integrator brief
//! shows the policy it reviewed under (ADR-032).
//!
//! `--report` writes the pulsation report (ADR-027 M6) and prints its path;
//! it needs `--stage integrator`, where the report becomes available.
//!
//! `--scope-request` makes the code unit read `src/util.rs` without owning it
//! and ask for it with `scope_insufficient` (ADR-035 M4); the fixture grants
//! the request at the failure checkpoint and goes on. `--stage scope` parks at
//! that checkpoint instead, with the request pending. `--stage done` approves
//! every checkpoint and waits for the signed record of the finished pulsation
//! (ADR-036), so the GUI only reads it.
use hvc_core::{
    AppCoreDependencies,
    feature_forge::{
        AcceptanceCriterion, CreatePulsationFromTasksRequest, CreateTasksRequest, TaskDraft,
        TaskKind, TaskSelection, TaskService,
    },
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::{
        PreparationSettings, SchemaV1,
        composition::settings::CompositionSettings,
        composition::{CheckKind, Handle, Uuid, ValidationCommandProfileV1},
        mandate::{SCOPE_GRANT_COMMAND_VERSION, ScopeGrantCommandV1},
        review_brief::failure_choices,
    },
    persistence_service::{
        ApplyHitlDecisionRequest, EnsureProjectForWorkspaceRequest,
        GetPulsationRuntimeSnapshotRequest, HitlDecision, PersistenceService,
        StageFailureCheckpoint,
    },
    workspace_service::bootstrap_greenfield_workspace,
};
use hvc_test_support::composition_replies::{
    PromptFacts, ReplyOptions, VISUALIZER, parse_prompt, unit_response, unit_response_with,
};
use hvc_test_support::pulsation_benchmark::{configure_loopback, force_api_direct};
use hvc_test_support::{
    FixedEmbeddingGateway, MemoryCredentialStore, ScriptedHttpServer, ScriptedResponse,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

// The parameter is not called `token`: `token: &Token` reads as a sensitive
// assignment to the secret redactor (ADR-030), whose edit guard then refuses
// the scripted whole-file replacement and parks Code.
const SOURCE: &str = "pub struct Token {\n    pub expires_at: u64,\n}\n\npub fn validate_token(candidate: &Token, now: u64) -> bool {\n    let _ = (candidate, now);\n    true\n}\n";
const UTIL_SOURCE: &str = "pub fn now() -> u64 {\n    0\n}\n";
const UTIL_IMPLEMENTED: &str = "pub fn now() -> u64 {\n    std::time::SystemTime::now()\n        .duration_since(std::time::UNIX_EPOCH)\n        .map_or(0, |elapsed| elapsed.as_secs())\n}\n";

struct Quiet;
impl LlmEventSink for Quiet {
    fn emit(&self, _: &LlmEventEnvelope) {}
}

fn criterion(id: &str, description: &str) -> AcceptanceCriterion {
    AcceptanceCriterion {
        id: id.into(),
        description: description.into(),
    }
}

fn tasks(scope_request: bool) -> Vec<TaskDraft> {
    let description = if scope_request {
        "validate_token in src/auth.rs accepts every token; compare against the clock in src/util.rs and add a unit test in tests/auth_test.rs"
    } else {
        "validate_token in src/auth.rs accepts every token; add the expiry check and a unit test in tests/auth_test.rs"
    };
    vec![
        TaskDraft {
            title: "Reject expired tokens".into(),
            kind: TaskKind::Bug,
            description: description.into(),
            criteria: vec![
                criterion("AC-1", "An expired token is rejected by validate_token"),
                criterion("AC-2", "A token that expires exactly now is rejected"),
            ],
            dependencies: vec![],
            feature_id: None,
        },
        TaskDraft {
            title: "Keep valid tokens working".into(),
            kind: TaskKind::Adjustment,
            description:
                "validate_token in src/auth.rs must keep accepting tokens that are still valid"
                    .into(),
            criteria: vec![criterion(
                "AC-3",
                "A token that expires in the future is still accepted",
            )],
            dependencies: vec![],
            feature_id: None,
        },
    ]
}

/// The default scripted reply, except that the Specification proposes one
/// code unit and one test unit after it, each covering every criterion, as
/// the host requires for a multi-task batch to plan Code and Test.
/// A realistic Visualizer note: a headline, one note per judgement item
/// (at most two) citing it, and the first item emphasised.
fn visualizer_reply(user_prompt: &str) -> Value {
    let input: Value = serde_json::from_str(user_prompt).expect("Visualizer input JSON");
    let items = input["judgement"].as_array().cloned().unwrap_or_default();
    let notes: Vec<Value> = items
        .iter()
        .take(2)
        .map(|item| {
            let reference = item["ref"].as_str().unwrap_or("J1");
            let text = match item["kind"].as_str() {
                Some("decision") => "This decision changes scope without a criterion of its own; confirm it matches what you asked for.",
                Some("reading_only_proof") => "Nothing will execute this proof; check that reading the code is enough for this criterion.",
                Some("interface_change") => "Code you already own changes shape here; check its callers before approving.",
                Some("unverified") => "The reviewer could not show this criterion holds; decide whether that is acceptable now.",
                _ => "Look at this item first; it is the one the host cannot settle for you.",
            };
            json!({"text": text, "refs": [reference]})
        })
        .collect();
    let headline = match items.len() {
        0 => "Nothing here needs your judgement beyond what the host already verified.".to_owned(),
        1 => "One point needs your judgement before this stage moves on.".to_owned(),
        count => format!("{count} points need your judgement; start with the first one."),
    };
    let emphasis: Vec<&str> = items
        .first()
        .and_then(|item| item["ref"].as_str())
        .into_iter()
        .collect();
    json!({"headline": headline, "notes": notes, "emphasis": emphasis})
}

fn reply(facts: &PromptFacts, fail_code: bool, scope_request: bool) -> Value {
    if facts.executor == VISUALIZER {
        return visualizer_reply(&facts.user_prompt);
    }
    if fail_code && facts.executor == "code_implementer" {
        return json!({
            "schemaVersion": 1, "outcome": "failed", "code": "contract_conflict",
            "message": "The approved signature cannot express the expiry input.", "relatedRefs": []
        });
    }
    // Until the grant, the code unit reads the clock helper it must change.
    if scope_request
        && facts.executor == "code_implementer"
        && facts
            .references
            .values()
            .any(|row| row.path == "src/util.rs" && !row.owned && !row.candidate && !row.planned)
    {
        return json!({
            "schemaVersion": 1, "outcome": "failed", "code": "scope_insufficient",
            "message": "now() in src/util.rs always returns 0, so validate_token cannot compare against the clock.",
            "relatedRefs": [facts.handle_for_path("src/util.rs")]
        });
    }
    if scope_request && facts.executor == "code_implementer" {
        let mut options = ReplyOptions::default();
        options
            .contents
            .insert("src/util.rs".into(), UTIL_IMPLEMENTED.into());
        return unit_response_with(facts, &options);
    }
    if facts.executor == "integration_reviewer" {
        // One verdict short of supported, so the Integrator brief has an item.
        let mut response = unit_response(facts);
        response["payload"]["assessments"][1]["verdict"] = json!("unverified");
        response["payload"]["assessments"][1]["explanation"] = json!(
            "No authored test sets expires_at equal to now, so the boundary is not shown either way."
        );
        return response;
    }
    if facts.executor != "specification_author" {
        return unit_response(facts);
    }
    let source = facts.handle_for_path("src/auth.rs");
    let test_destination = facts
        .references
        .iter()
        .find(|(_, row)| row.path == "tests/auth_test.rs" && row.planned)
        .map(|(handle, _)| handle.as_str())
        .unwrap_or(source);
    let criteria: Vec<&str> = facts
        .criteria
        .iter()
        .map(|(_, criterion)| criterion.as_str())
        .collect();
    let mut code_inputs = vec![source];
    if scope_request {
        code_inputs.push(facts.handle_for_path("src/util.rs"));
    }
    let options = ReplyOptions {
        proposals: Some(json!([
            {
                "unitKey": "implement_expiry", "kind": "code", "taskRefs": facts.tasks,
                "criterionRefs": criteria,
                "inputRefs": code_inputs, "destinationRefs": [source], "dependsOn": [],
                "objective": "Implement the expiry comparison in validate_token."
            },
            {
                "unitKey": "cover_expiry", "kind": "test", "taskRefs": facts.tasks,
                "criterionRefs": criteria,
                "inputRefs": [source], "destinationRefs": [test_destination],
                "dependsOn": ["implement_expiry"],
                "objective": "Cover expired and boundary tokens with unit tests."
            }
        ])),
        ..ReplyOptions::default()
    };
    let mut response = unit_response_with(facts, &options);
    if facts.user_prompt.contains(REVISION_MARK) {
        revise(&mut response["payload"]);
    }
    response
}

const REVISION_MARK: &str = "Revision feedback from the developer:";

const FEEDBACK: &str = "Flagged by the developer in the review brief:
- [Decision] D1: Keep the public signature of validate_token. — Also decide what happens when the token expires exactly now.
- [Proof by reading only] AC-2: A token that expires exactly now is rejected — Say which boundary test proves it.

Additional feedback: rename the test unit after what it covers.";

/// What the scripted author changes when it answers `FEEDBACK`.
fn revise(payload: &mut Value) {
    payload["decisions"] = json!([
        "Keep the public signature of validate_token.",
        "Treat a token that expires exactly now as expired."
    ]);
    payload["requirements"][1]["requirement"] =
        json!("validate_token returns false when expires_at equals now.");
    payload["requirements"][1]["verificationIntent"] =
        json!("Author a boundary test with expires_at equal to now.");
    payload["exclusions"]
        .as_array_mut()
        .unwrap()
        .push(json!("No clock-skew tolerance."));
    payload["unitProposals"][1]["unitKey"] = json!("cover_boundaries");
    payload["unitProposals"][1]["objective"] =
        json!("Cover expired, boundary and still-valid tokens with unit tests.");
}

/// Waits for a waiting checkpoint of `kind` (at `stage`, for an artifact
/// review) other than `not`, and returns its id.
fn wait_for_checkpoint(
    database: &rusqlite::Connection,
    pulse: &str,
    kind: &str,
    stage: &str,
    not: Option<&str>,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let waiting: Option<String> = database
            .query_row(
                "SELECT c.id FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.status='checkpoint' AND c.status='waiting' AND c.kind=?2 AND (?2<>'artifact_review' OR p.stage=?3) AND c.id<>?4",
                [pulse, kind, stage, not.unwrap_or("")],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = waiting {
            return id;
        }
        let status: String = database
            .query_row(
                "SELECT status FROM pulsations WHERE id=?1",
                [pulse],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            status != "failed" && Instant::now() < deadline,
            "pulsation did not reach the {stage} {kind} checkpoint (status {status})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn approve(service: &PersistenceService, pulse: &str) {
    let checkpoint = service.get_hitl_checkpoint(pulse).unwrap();
    // A high-risk review approves with an acknowledgement (ADR-034 M3); the
    // host ignores it everywhere else.
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
            Some(hvc_core::persistence_service::RiskAcknowledgement {
                reason: "Fixture approval.".into(),
            }),
        )
        .unwrap();
}

/// The waiting stage failure of the fixture's project.
fn waiting_failure(service: &PersistenceService, project: &str) -> StageFailureCheckpoint {
    service
        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
            project_id: project.into(),
        })
        .unwrap()
        .snapshot
        .failure_checkpoints
        .into_iter()
        .next()
        .expect("a waiting stage failure")
}

/// Grants the pending scope request with what the card shows.
fn grant_scope(service: &PersistenceService, project: &str) {
    let failure = waiting_failure(service, project);
    let request = failure
        .scope_request
        .as_ref()
        .expect("the failure carries a scope request");
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
        .unwrap();
}

/// Waits until `query` on the pulsation returns true.
fn wait_for(database: &rusqlite::Connection, pulse: &str, what: &str, query: &str) {
    let deadline = Instant::now() + Duration::from_secs(180);
    while !database
        .query_row(query, [pulse], |row| row.get::<_, bool>(0))
        .unwrap()
    {
        assert!(Instant::now() < deadline, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A required compile check run by the `check_fixture` stand-in, when it was
/// built next to the examples folder (`cargo build -p hvc-test-support --bins`).
fn passing_check() -> Option<ValidationCommandProfileV1> {
    let examples = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let executable = examples
        .parent()?
        .join(format!("check_fixture{}", std::env::consts::EXE_SUFFIX));
    executable.is_file().then(|| ValidationCommandProfileV1 {
        schema_version: SchemaV1,
        profile_id: Uuid::generate(),
        profile_version: 1,
        check_ref: Handle::new("fixture_compile").unwrap(),
        kind: CheckKind::Compile,
        required: true,
        executable: executable.to_string_lossy().into_owned(),
        argv: vec!["pass".into()],
        cwd_relative_path: ".".into(),
        input_globs: vec!["**/*.rs".into()],
        environment_names: vec![],
        toolchain_identity: "fixture".into(),
        timeout_seconds: 30,
        max_log_bytes: 65_536,
    })
}

const USAGE: &str = "usage: review_brief_fixture NEW_DIRECTORY [--revised] [--visualizer] [--report] [--review-policy] [--scope-request] [--stage contract|test|integrator|failure|scope|done]";

fn main() {
    let root = PathBuf::from(std::env::args_os().nth(1).expect(USAGE));
    let mut revised = false;
    let mut visualizer = false;
    let mut report = false;
    let mut review_policy = false;
    let mut scope_request = false;
    let mut target = "specification".to_owned();
    let mut options = std::env::args().skip(2);
    while let Some(option) = options.next() {
        match option.as_str() {
            "--revised" => revised = true,
            "--visualizer" => visualizer = true,
            "--report" => report = true,
            "--review-policy" => review_policy = true,
            "--scope-request" => scope_request = true,
            "--stage" => target = options.next().expect(USAGE),
            other => panic!("unknown option {other}; {USAGE}"),
        }
    }
    let later: &[&str] = match target.as_str() {
        "specification" => &[],
        "contract" => &["Contract"],
        "test" => &["Contract", "Test"],
        "integrator" => &["Contract", "Test", "Integrator"],
        "failure" | "scope" => &["Contract"],
        "done" => &["Contract", "Test", "Integrator"],
        other => panic!("unknown stage {other}; {USAGE}"),
    };
    let fail_code = target == "failure";
    let park_at_scope = target == "scope";
    let scope_request = scope_request || park_at_scope;
    let parks_at_failure = fail_code || park_at_scope;
    let done = target == "done";
    assert!(!root.exists(), "refuse to reuse or overwrite {root:?}");
    fs::create_dir_all(&root).unwrap();
    // The GUI registers projects by their canonical root (`\\?\` form on
    // Windows); any other spelling would restore as a second, empty project.
    let root = root.canonicalize().unwrap();
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    // Follow the real host's bootstrap so the GUI can restore the session.
    bootstrap_greenfield_workspace(&workspace).unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/auth.rs"), SOURCE).unwrap();
    if scope_request {
        fs::write(workspace.join("src/util.rs"), UTIL_SOURCE).unwrap();
    }
    if review_policy {
        fs::write(
            workspace.join("REVIEW.md"),
            "# Review policy\n\nTreat a token that expires exactly now as expired.\nFlag any change to the public signature of validate_token.\n",
        )
        .unwrap();
    }

    let server = ScriptedHttpServer::respond_with(move |request| {
        if request.body.trim().is_empty() {
            return ScriptedResponse::json(400, r#"{"error":"empty request"}"#);
        }
        let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
        let model = body["model"]
            .as_str()
            .expect("provider request model identity");
        let content = reply(&parse_prompt(request), fail_code, scope_request);
        ScriptedResponse::json(
            200,
            json!({"model":model,"choices":[{"message":{"content":content.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":60}}).to_string(),
        )
    });
    let data = root.join("data");
    let deps = AppCoreDependencies::default();
    let service = PersistenceService::initialize_with_dependencies(
        &data,
        Arc::new(Quiet),
        Arc::new(MemoryCredentialStore::default()),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    configure_loopback(&service, &server);
    rusqlite::Connection::open(service.database_path())
        .unwrap()
        .execute(
            "UPDATE provider_connections SET credential_ref='review-brief-fixture:' || id",
            [],
        )
        .unwrap();
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: false,
            learning_enabled: false,
            ..PreparationSettings::default()
        })
        .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.to_str().unwrap().into(),
            preferred_name: Some("Review brief fixture".into()),
        })
        .unwrap()
        .project
        .id;
    force_api_direct(&service, &project);
    let check = passing_check();
    if let Some(profile) = check.clone() {
        service
            .upsert_validation_profile(&project, profile, true)
            .unwrap();
    }
    service
        .update_composition_settings(
            &project,
            CompositionSettings {
                enabled: true,
                validation_enabled: check.is_some(),
                visualizer_enabled: visualizer,
                ..CompositionSettings::default()
            }
            .with_all_recipes_qualified(),
        )
        .unwrap();
    let created = TaskService::new(service.clone())
        .create(CreateTasksRequest {
            project_id: project.clone(),
            request_id: "review-brief-fixture-tasks".into(),
            tasks: tasks(scope_request),
        })
        .unwrap();
    let pulse = service
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project.clone(),
            request_id: "review-brief-fixture-dispatch".into(),
            title: "Token expiry".into(),
            tasks: created
                .iter()
                .map(|task| TaskSelection {
                    task_id: task.id.clone(),
                    expected_revision: task.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let stop = CancellationToken::new();
    let worker = service.clone();
    let token = stop.clone();
    runtime.spawn(async move { worker.run_job_worker_until(token).await });
    let database = rusqlite::Connection::open(service.database_path()).unwrap();
    let first = wait_for_checkpoint(&database, &pulse, "artifact_review", "SPECIFICATION", None);
    if revised {
        let checkpoint = service.get_hitl_checkpoint(&pulse).unwrap();
        assert_eq!(checkpoint.checkpoint_id, first);
        service
            .apply_hitl_decision(ApplyHitlDecisionRequest {
                pulsation_id: pulse.clone(),
                checkpoint_id: checkpoint.checkpoint_id,
                expected_state_version: checkpoint.state_version,
                feedback: Some(FEEDBACK.into()),
                decision: HitlDecision::RequestRevision,
            })
            .unwrap();
        wait_for_checkpoint(
            &database,
            &pulse,
            "artifact_review",
            "SPECIFICATION",
            Some(&first),
        );
    }
    for stage in later {
        approve(&service, &pulse);
        if scope_request && *stage == "Test" {
            // Code asked for the clock helper; grant it and let the retry run.
            wait_for_checkpoint(&database, &pulse, "stage_failure", "", None);
            grant_scope(&service, &project);
        }
        wait_for_checkpoint(&database, &pulse, "artifact_review", stage, None);
    }
    if parks_at_failure {
        approve(&service, &pulse);
        wait_for_checkpoint(&database, &pulse, "stage_failure", "", None);
    }
    if done {
        approve(&service, &pulse);
        wait_for(
            &database,
            &pulse,
            "the pulsation's end",
            "SELECT status='done' FROM pulsations WHERE id=?1",
        );
        wait_for(
            &database,
            &pulse,
            "the seal",
            "SELECT EXISTS(SELECT 1 FROM attestation_seal_intents WHERE pulsation_id=?1 AND state<>'pending')",
        );
    }
    if visualizer && !parks_at_failure && !done {
        // Let the note of the parked checkpoint be written before the worker
        // stops; the GUI only reads it.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let done: bool = database
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM checkpoint_visualizations v JOIN checkpoints c ON c.id=v.checkpoint_id WHERE c.pulsation_id=?1 AND c.status='waiting' AND v.state IN ('validated','omitted'))",
                    [&pulse],
                    |row| row.get(0),
                )
                .unwrap();
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the Visualizer note did not arrive"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    stop.cancel();
    runtime.shutdown_timeout(Duration::from_secs(5));

    service
        .save_ui_session(
            serde_json::from_value(json!({
                "projectId": project,
                "workspaceRoot": workspace,
                "workspaceViewMode": "editor",
                "activeView": "pulsations",
                "activePulsationId": pulse,
            }))
            .unwrap(),
        )
        .unwrap();
    if parks_at_failure {
        let failure = waiting_failure(&service, &project);
        println!(
            "Failure: {} at {:?}, {} choices",
            failure.failure_class,
            failure.stage,
            failure_choices(&failure).len()
        );
        if let Some(request) = &failure.scope_request {
            println!(
                "Scope request: unit {}, {} file(s) accepted, {} refused, risk {:?} -> {:?}",
                request.unit_key,
                request.accepted.len(),
                request.refused.len(),
                request.risk_before,
                request.risk_after
            );
        }
    } else if done {
        let attestation = service.attestation_status(&pulse).unwrap();
        println!(
            "Attestation: {:?}, outcome {}, {} subject(s)",
            attestation.state,
            attestation.outcome.as_deref().unwrap_or("-"),
            attestation.subject_count
        );
    } else {
        let checkpoint = service.get_hitl_checkpoint(&pulse).unwrap();
        let brief = service
            .read_checkpoint_brief(&checkpoint)
            .unwrap()
            .expect("an artifact checkpoint of a v4 pulsation has a brief");
        println!(
            "Brief: {:?}, {} judgement items, counts {:?}",
            brief.stage,
            brief.judgement.len(),
            brief.counts
        );
        if let Some(diff) = &brief.revision {
            println!(
                "Revision: {} changes since the previous candidate",
                diff.changes.as_ref().map_or(0, Vec::len)
            );
        }
    }
    println!(
        "Host check: {}",
        if check.is_some() {
            "fixture_compile (required)"
        } else {
            "none (build check_fixture first)"
        }
    );
    if report {
        let path = service.write_pulsation_report(&pulse).unwrap();
        println!("Report: {}", path.display());
    }
    println!("HVC_DATA_DIR={}", data.display());
}
