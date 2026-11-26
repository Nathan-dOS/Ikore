//! Feature 25 (M4): unit & core tests for stage verifiers store, prompts,
//! failure decision matrix, admission rules, crash recovery, rejection limits,
//! advisory verifiers, Continue anyway justification, and `verification_error`
//! checkpoints (S2).

use rusqlite::{Connection, TransactionBehavior, params};

use super::{
    AgentDockSession, CreateProjectRequest, PersistenceService, PersistenceServiceError, Pulsation,
    PulsationStage, PulsationStatus, ResolveStageFailureRequest, StageFailureDecision,
};
use crate::custom_pipelines::{StageVerdictV1, StepAgent, StepTransport, Verdict};
use crate::orchestration::FailureClass;
use crate::stage_verifiers::{
    FindingSeverity, MAX_STAGE_RESULT_INLINE_BYTES, MAX_VERIFIERS_PER_PROJECT,
    MAX_VERIFIERS_PER_STAGE, ResolveVerificationErrorRequest, SaveStageVerifierOutcome,
    StageVerifierConfig, StageVerifierDraft, VerdictFinding, VerificationErrorDecision,
    prompt::{
        AcceptedStageArtifact, load_latest_rejected_findings, verifier_correction_prompt,
        verifier_system_prompt, verifier_user_prompt,
    },
};

struct Fixture {
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    service: PersistenceService,
    project: String,
    workspace_path: String,
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let workspace_path = workspace.path().to_string_lossy().into_owned();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = service
        .create_project(CreateProjectRequest {
            workspace_path: workspace_path.clone(),
            name: Some("Verifiers fixture".into()),
        })
        .unwrap()
        .project
        .id;
    Fixture {
        _data: data,
        _workspace: workspace,
        service,
        project,
        workspace_path,
    }
}

fn enable_labs(f: &Fixture, enabled: bool) {
    let mut settings = f.service.get_composition_settings(&f.project).unwrap();
    settings.stage_verifiers_enabled = enabled;
    f.service
        .update_composition_settings(&f.project, settings)
        .unwrap();
}

fn draft(stage: &str, label: &str, position: i64, reject_limit: u8) -> StageVerifierDraft {
    StageVerifierDraft {
        stage: stage.into(),
        label: label.into(),
        position,
        enabled: true,
        reject_limit,
        agent: StepAgent {
            transport: StepTransport::Cli,
            backend: Some("claude".into()),
            model: Some("haiku".into()),
            model_profile: None,
            thinking_level: "default".into(),
            api_route_id: None,
        },
        instructions: "Inspect the stage artifacts carefully.".into(),
    }
}

fn create_verifier(
    f: &Fixture,
    stage: &str,
    label: &str,
    position: i64,
    reject_limit: u8,
) -> String {
    match f
        .service
        .create_stage_verifier(&f.project, draft(stage, label, position, reject_limit))
        .unwrap()
    {
        SaveStageVerifierOutcome::Saved { verifier } => verifier.id,
        SaveStageVerifierOutcome::Invalid { errors } => panic!("unexpected invalid: {errors:?}"),
    }
}

fn codes(outcome: SaveStageVerifierOutcome) -> Vec<String> {
    match outcome {
        SaveStageVerifierOutcome::Saved { .. } => Vec::new(),
        SaveStageVerifierOutcome::Invalid { errors } => {
            errors.into_iter().map(|e| e.code).collect()
        }
    }
}

fn seed_running_pulsation(
    db: &Connection,
    project_id: &str,
    pulsation_id: &str,
    stage: PulsationStage,
    status: PulsationStatus,
    state_version: u64,
) -> String {
    let now = "2026-11-25T12:00:00Z";
    let job_id = format!("job-{pulsation_id}-g{state_version}");
    db.execute(
        "INSERT INTO pulsations(
           id,project_id,feature_name,stage,status,state_version,input_revision,
           queue_sequence,created_at,updated_at
         ) VALUES(
           ?1,?2,'Feature X',?3,?4,?5,1,
           (SELECT COALESCE(MAX(queue_sequence),0)+1 FROM pulsations WHERE project_id=?2),?6,?6
         )
         ON CONFLICT(id) DO UPDATE SET
           stage=excluded.stage,status=excluded.status,
           state_version=excluded.state_version,updated_at=excluded.updated_at",
        params![
            pulsation_id,
            project_id,
            stage.as_db_value(),
            status.as_db_value(),
            state_version as i64,
            now
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO jobs(
           id,project_id,pulsation_id,kind,status,attempts,
           input_revision,generation,state_version,idempotency_key,created_at,updated_at
         ) VALUES(?1,?2,?3,?4,'done',0,1,1,?5,?1,?6,?6)",
        params![
            job_id,
            project_id,
            pulsation_id,
            format!("run_stage::{}", stage.as_db_value()),
            state_version as i64,
            now
        ],
    )
    .unwrap();
    job_id
}

fn pulse_row(db: &Connection, pulsation_id: &str) -> Pulsation {
    PersistenceService::find_pulsation_by_id(db, pulsation_id)
        .unwrap()
        .expect("pulsation")
}

fn reject_verdict(summary: &str, blocking_msg: &str, file: Option<&str>) -> StageVerdictV1 {
    StageVerdictV1 {
        verdict: Verdict::Reject,
        summary: summary.into(),
        findings: vec![VerdictFinding {
            id: "F1".into(),
            severity: FindingSeverity::Blocking,
            message: blocking_msg.into(),
            file: file.map(str::to_owned),
            line: file.map(|_| 12),
            suggestion: None,
            file_not_found: false,
        }],
    }
}

fn pass_verdict(summary: &str, warning_msg: Option<&str>) -> StageVerdictV1 {
    StageVerdictV1 {
        verdict: Verdict::Pass,
        summary: summary.into(),
        findings: warning_msg
            .map(|msg| VerdictFinding {
                id: "F1".into(),
                severity: FindingSeverity::Warning,
                message: msg.into(),
                file: None,
                line: None,
                suggestion: None,
                file_not_found: false,
            })
            .into_iter()
            .collect(),
    }
}

fn mark_latest_verifier_run_verdict(
    db: &Connection,
    pulsation_id: &str,
    verifier_id: &str,
    verdict: &StageVerdictV1,
) {
    let status = match verdict.verdict {
        Verdict::Pass => "passed",
        Verdict::Reject => "rejected",
    };
    let verdict_json = serde_json::to_string(verdict).unwrap();
    let run_id: String = db
        .query_row(
            "SELECT id FROM stage_verifier_runs
              WHERE pulsation_id=?1 AND verifier_id=?2
              ORDER BY stage_generation DESC,attempt DESC LIMIT 1",
            params![pulsation_id, verifier_id],
            |row| row.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE stage_verifier_runs
            SET status=?2,verdict_json=?3,finished_at='2026-11-25T12:05:00Z'
          WHERE id=?1",
        params![run_id, status, verdict_json],
    )
    .unwrap();
}

#[test]
fn verifier_crud_caps_trimmed_uniqueness_nul_delete_restrict_and_versioning() {
    let f = fixture();
    // Trimmed label on create & case-insensitive uniqueness on update/create.
    let v1 = create_verifier(&f, "Code", "  Security gate  ", 0, 2);
    let list = f.service.list_stage_verifiers(&f.project).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].label, "Security gate");
    assert_eq!(list[0].version, 1);

    let dup = f
        .service
        .create_stage_verifier(&f.project, draft("Code", " security GATE ", 1, 2))
        .unwrap();
    assert_eq!(codes(dup), vec!["label_taken"]);

    // NUL bytes rejected in label and instructions.
    let mut bad = draft("Code", "Null\0check", 1, 2);
    bad.instructions = "ok\0bad".into();
    let nul_codes = codes(f.service.create_stage_verifier(&f.project, bad).unwrap());
    assert!(nul_codes.contains(&"label_invalid".to_string()));
    assert!(nul_codes.contains(&"instructions_invalid".to_string()));

    // Stage cap = 5.
    for i in 2..=MAX_VERIFIERS_PER_STAGE {
        create_verifier(&f, "Code", &format!("V{i}"), i as i64, 1);
    }
    let stage_cap = f
        .service
        .create_stage_verifier(&f.project, draft("Code", "V6", 6, 1))
        .unwrap();
    assert_eq!(codes(stage_cap), vec!["stage_limit_reached"]);

    // Project cap = 15 across remaining stages.
    let stages = ["Intake", "SPECIFICATION", "Contract", "Test", "Integrator"];
    let mut created = MAX_VERIFIERS_PER_STAGE;
    'outer: for st in stages {
        for j in 0..MAX_VERIFIERS_PER_STAGE {
            if created >= MAX_VERIFIERS_PER_PROJECT {
                break 'outer;
            }
            create_verifier(&f, st, &format!("{st}-{j}"), j as i64, 1);
            created += 1;
        }
    }
    assert_eq!(created, MAX_VERIFIERS_PER_PROJECT);
    let proj_cap = f
        .service
        .create_stage_verifier(&f.project, draft("Integrator", "Overflow", 5, 1))
        .unwrap();
    assert_eq!(codes(proj_cap), vec!["project_limit_reached"]);

    // Delete works when run_count == 0, then record a run on v1 and assert ON DELETE RESTRICT.
    let extra_id = f
        .service
        .list_stage_verifiers(&f.project)
        .unwrap()
        .into_iter()
        .find(|v| v.label == "V2")
        .unwrap()
        .id;
    f.service
        .delete_stage_verifier(&f.project, &extra_id)
        .unwrap();

    let db = Connection::open(f.service.database_path()).unwrap();
    let job_id = seed_running_pulsation(
        &db,
        &f.project,
        "p-crud",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let cfg = StageVerifierConfig {
        verifier_id: v1.clone(),
        version: 1,
        stage: "Code".into(),
        label: "Security gate".into(),
        position: 0,
        reject_limit: 2,
        agent: draft("Code", "Security gate", 0, 2).agent,
        instructions: "Initial instructions".into(),
    };
    let cfg_json = serde_json::to_string(&cfg).unwrap();
    db.execute(
        "INSERT INTO stage_verifier_runs(
           id,verifier_id,config_json,pulsation_id,stage,stage_job_id,stage_generation,attempt,recovery_count,status,created_at
         ) VALUES('run-1',?1,?2,'p-crud','Code',?3,1,1,0,'passed','2026-11-25T12:00:00Z')",
        params![v1, cfg_json, job_id],
    )
    .unwrap();

    let del_err = f
        .service
        .delete_stage_verifier(&f.project, &v1)
        .unwrap_err();
    assert!(matches!(del_err, PersistenceServiceError::Validation(_)));
    let sql_del = db.execute("DELETE FROM stage_verifiers WHERE id=?1", [&v1]);
    assert!(sql_del.is_err(), "ON DELETE RESTRICT must block SQL delete");

    // Update bumps version while old run preserves historical config_json.
    let mut updated_draft = draft("Code", "Security gate v2", 0, 3);
    updated_draft.instructions = "New instructions".into();
    match f
        .service
        .update_stage_verifier(&f.project, &v1, updated_draft)
        .unwrap()
    {
        SaveStageVerifierOutcome::Saved { verifier } => assert_eq!(verifier.version, 2),
        other => panic!("expected saved: {other:?}"),
    }
    let stored_cfg: String = db
        .query_row(
            "SELECT config_json FROM stage_verifier_runs WHERE id='run-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored_cfg.contains("Initial instructions"));

    // Can disable instead of delete.
    let toggled = f
        .service
        .set_stage_verifier_enabled(&f.project, &v1, false)
        .unwrap();
    assert!(!toggled.enabled);
    drop(db);
}

#[test]
fn agent_dock_accepts_verifier_identifier() {
    let ok = AgentDockSession {
        agent: Some("verifier:v-123".into()),
        mode: None,
        height: None,
        follow: false,
    };
    assert!(ok.validate().is_ok());

    let empty = AgentDockSession {
        agent: Some("verifier:".into()),
        mode: None,
        height: None,
        follow: false,
    };
    assert!(empty.validate().is_err());

    let ctrl = AgentDockSession {
        agent: Some("verifier:v\n1".into()),
        mode: None,
        height: None,
        follow: false,
    };
    assert!(ctrl.validate().is_err());
}

#[test]
fn prompts_follow_budget_provenance_previous_verdict_and_correction_contract() {
    let cfg = StageVerifierConfig {
        verifier_id: "ver-1".into(),
        version: 1,
        stage: "Code".into(),
        label: "Spec checker".into(),
        position: 0,
        reject_limit: 2,
        agent: draft("Code", "Spec checker", 0, 2).agent,
        instructions: "Check for edge cases.".into(),
    };
    let sys = verifier_system_prompt(&cfg);
    assert!(sys.contains("Spec checker"));
    assert!(sys.contains("Code"));
    assert!(sys.contains("TOOLS: You are running read-only."));
    assert!(sys.contains("PROVENANCE: An item's"));

    let small = AcceptedStageArtifact {
        name: "src/lib.rs".into(),
        kind: "code".into(),
        relative_path: Some("src/lib.rs".into()),
        content_sha256: "sha-small".into(),
        content: "pub fn ok() {}".into(),
    };
    let huge = AcceptedStageArtifact {
        name: "src/huge.rs".into(),
        kind: "code".into(),
        relative_path: Some("src/huge.rs".into()),
        content_sha256: "sha-huge".into(),
        content: "x".repeat(MAX_STAGE_RESULT_INLINE_BYTES + 128),
    };
    let prev = reject_verdict("Missing check", "Handle empty input", Some("src/lib.rs"));

    let gen1 = verifier_user_prompt(
        &cfg,
        PulsationStage::Code,
        &[small.clone(), huge.clone()],
        Some("- task_1 (modify): Add parser"),
        None,
    );
    assert!(gen1.contains("> provenance: human"));
    assert!(gen1.contains("verifier definition"));
    assert!(gen1.contains("## Developer instructions"));
    assert!(gen1.contains("## Stage result"));
    assert!(gen1.contains("src/lib.rs"));
    assert!(gen1.contains("Additional artifacts (paths and digests)"));
    assert!(gen1.contains("sha-huge"));
    assert!(gen1.contains("## Task scope"));
    assert!(!gen1.contains("## Your previous verdict"));

    let gen2 = verifier_user_prompt(&cfg, PulsationStage::Code, &[small], None, Some(&prev));
    assert!(gen2.contains("## Your previous verdict"));
    assert!(gen2.contains("agent_output"));
    assert!(gen2.contains("stage/verifier"));
    assert!(gen2.contains("Handle empty input"));

    let correction = verifier_correction_prompt(&gen2, "not json", "expected object");
    assert!(correction.contains("## Your previous answer"));
    assert!(correction.contains("attempt 1"));
    assert!(correction.contains("rejected"));
}

#[test]
fn failure_matrix_offers_continue_anyway_only_for_verification_rejected() {
    use crate::orchestration::offered_decisions;
    for stage in [
        PulsationStage::Intake,
        PulsationStage::Specification,
        PulsationStage::Contract,
        PulsationStage::Code,
        PulsationStage::Test,
        PulsationStage::Integrator,
    ] {
        let offered =
            offered_decisions(FailureClass::VerificationRejected, stage).expect("offered");
        assert!(offered.allow_retry);
        assert!(offered.allow_revision);
        assert!(offered.allow_continue_anyway);

        let v4 = crate::orchestration::offered_decisions_v4(
            FailureClass::VerificationRejected,
            stage,
            0,
            false,
        )
        .expect("v4 offered");
        assert!(v4.allow_continue_anyway);
        if matches!(stage, PulsationStage::Test | PulsationStage::Integrator) {
            assert_eq!(v4.revision_targets, vec![stage, PulsationStage::Code]);
        } else {
            assert_eq!(v4.revision_targets, vec![stage]);
        }
    }

    let other =
        offered_decisions(FailureClass::ValidationFailed, PulsationStage::Code).expect("offered");
    assert!(!other.allow_continue_anyway);
}

#[test]
fn labs_off_default_unchanged_even_with_configured_verifiers() {
    let f = fixture();
    let _v = create_verifier(&f, "Code", "Code gate", 0, 2);
    enable_labs(&f, false);

    let mut db = Connection::open(f.service.database_path()).unwrap();
    let job_id = seed_running_pulsation(
        &db,
        &f.project,
        "p-labs-off",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let current = pulse_row(&db, "p-labs-off");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let started = PersistenceService::maybe_start_stage_verification_in_tx(
        &tx,
        &current,
        &job_id,
        1,
        1,
        "advance:Test",
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(!started);
    let runs: i64 = db
        .query_row("SELECT count(*) FROM stage_verifier_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 0);
}

#[test]
fn reject_once_requeues_with_findings_then_pass_executes_deferred_disposition() {
    let f = fixture();
    enable_labs(&f, true);
    let v = create_verifier(&f, "Code", "Code gate", 0, 2);
    let mut db = Connection::open(f.service.database_path()).unwrap();
    let job1 = seed_running_pulsation(
        &db,
        &f.project,
        "p-requeue",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let current = pulse_row(&db, "p-requeue");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(
        PersistenceService::maybe_start_stage_verification_in_tx(
            &tx,
            &current,
            &job1,
            1,
            1,
            "advance:Test",
        )
        .unwrap()
    );
    tx.commit().unwrap();

    // Mark the verifier run rejected (count = 1 <= limit 2 -> auto re-run).
    mark_latest_verifier_run_verdict(
        &db,
        "p-requeue",
        &v,
        &reject_verdict("Tests missing", "Cover empty case", Some("src/lib.rs")),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-requeue",
        PulsationStage::Code,
        1,
        "2026-11-25T12:06:00Z",
    )
    .unwrap();
    tx.commit().unwrap();

    let after_reject = pulse_row(&db, "p-requeue");
    assert_eq!(after_reject.stage, PulsationStage::Code);
    assert_eq!(after_reject.status, PulsationStatus::Planning);

    // Gen 2 reads rejected findings.
    let findings =
        load_latest_rejected_findings(&db, "p-requeue", PulsationStage::Code, 2).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].verifier_label, "Code gate");
    assert_eq!(findings[0].findings[0].message, "Cover empty case");

    // Simulate Gen 2 stage running & finishing, then passing verification.
    let job2 = seed_running_pulsation(
        &db,
        &f.project,
        "p-requeue",
        PulsationStage::Code,
        PulsationStatus::Running,
        3,
    );
    let current2 = pulse_row(&db, "p-requeue");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(
        PersistenceService::maybe_start_stage_verification_in_tx(
            &tx,
            &current2,
            &job2,
            2,
            1,
            "advance:Test",
        )
        .unwrap()
    );
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(
        &db,
        "p-requeue",
        &v,
        &pass_verdict("Looks good now", Some("Consider adding doc comments")),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-requeue",
        PulsationStage::Code,
        2,
        "2026-11-25T12:10:00Z",
    )
    .unwrap();
    tx.commit().unwrap();

    let advanced = pulse_row(&db, "p-requeue");
    assert_eq!(advanced.stage, PulsationStage::Test);
    assert_eq!(advanced.status, PulsationStatus::Planning);

    // Checkpoints carryover: Code's non-blocking Warning carries over to Test.
    let carried = f
        .service
        .list_checkpoint_verification_findings("p-requeue", PulsationStage::Test)
        .unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(
        carried[0].verdict.as_ref().unwrap().findings[0].message,
        "Consider adding doc comments"
    );
}

#[test]
fn reject_limit_exhausted_retry_revise_continue_anyway_and_counter_isolation() {
    let f = fixture();
    enable_labs(&f, true);
    let v = create_verifier(&f, "SPECIFICATION", "Spec gate", 0, 1);
    let mut db = Connection::open(f.service.database_path()).unwrap();

    // Rejection 1 (limit=1 -> auto re-run).
    let job1 = seed_running_pulsation(
        &db,
        &f.project,
        "p-limit",
        PulsationStage::Specification,
        PulsationStatus::Running,
        1,
    );
    let p1 = pulse_row(&db, "p-limit");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(&tx, &p1, &job1, 1, 1, "await_review")
        .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(
        &db,
        "p-limit",
        &v,
        &reject_verdict("Unclear AC", "Clarify timeout requirement", None),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-limit",
        PulsationStage::Specification,
        1,
        "2026-11-25T12:02:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(pulse_row(&db, "p-limit").status, PulsationStatus::Planning);

    // Rejection 2 (count=2 > limit=1 -> opens VerificationRejected checkpoint).
    let job2 = seed_running_pulsation(
        &db,
        &f.project,
        "p-limit",
        PulsationStage::Specification,
        PulsationStatus::Running,
        3,
    );
    let p2 = pulse_row(&db, "p-limit");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(&tx, &p2, &job2, 2, 1, "await_review")
        .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(
        &db,
        "p-limit",
        &v,
        &reject_verdict("Still unclear", "AC2 contradicts AC1", None),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-limit",
        PulsationStage::Specification,
        2,
        "2026-11-25T12:05:00Z",
    )
    .unwrap();
    tx.commit().unwrap();

    let snap = f
        .service
        .get_pulsation_runtime_snapshot(super::GetPulsationRuntimeSnapshotRequest {
            project_id: f.project.clone(),
        })
        .unwrap()
        .snapshot;
    assert_eq!(snap.failure_checkpoints.len(), 1);
    let cp = &snap.failure_checkpoints[0];
    assert_eq!(cp.failure_class, "verification_rejected");
    assert!(cp.allow_retry);
    assert!(cp.allow_revision);
    assert!(cp.allow_continue_anyway);

    // Decision 1: Retry gives +1 round without resetting counter, and is excluded from retry convergence.
    let res = f
        .service
        .resolve_stage_failure_with_convergence(
            ResolveStageFailureRequest {
                checkpoint_id: cp.checkpoint_id.clone(),
                expected_state_version: cp.state_version,
                decision: StageFailureDecision::RetryStage,
                feedback: None,
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(res.pulsation.status, PulsationStatus::Planning);

    let convergence = PersistenceService::convergence_status_on(&db, "p-limit").unwrap();
    assert_eq!(
        convergence.retries, 0,
        "verification_rejected Retry must stay isolated from stage failure retries"
    );

    // Next rejection (gen 3) STILL opens failure checkpoint because Retry did not reset counter.
    let job3 = seed_running_pulsation(
        &db,
        &f.project,
        "p-limit",
        PulsationStage::Specification,
        PulsationStatus::Running,
        6,
    );
    let p3 = pulse_row(&db, "p-limit");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(&tx, &p3, &job3, 3, 1, "await_review")
        .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(
        &db,
        "p-limit",
        &v,
        &reject_verdict("Retry failed too", "Still bad", None),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-limit",
        PulsationStage::Specification,
        3,
        "2026-11-25T12:08:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    let cp2 = f
        .service
        .get_pulsation_runtime_snapshot(super::GetPulsationRuntimeSnapshotRequest {
            project_id: f.project.clone(),
        })
        .unwrap()
        .snapshot
        .failure_checkpoints
        .into_iter()
        .next()
        .expect("checkpoint reopened after retry");

    // Justification > 500 chars MUST be rejected while keeping checkpoint waiting.
    let too_long = "a".repeat(501);
    let err = f
        .service
        .resolve_stage_failure_with_convergence(
            ResolveStageFailureRequest {
                checkpoint_id: cp2.checkpoint_id.clone(),
                expected_state_version: cp2.state_version,
                decision: StageFailureDecision::ContinueAnyway,
                feedback: Some(too_long),
            },
            None,
            None,
        )
        .unwrap_err();
    assert!(matches!(err, PersistenceServiceError::Validation(_)));
    let still_waiting: String = db
        .query_row(
            "SELECT status FROM checkpoints WHERE id=?1",
            [&cp2.checkpoint_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still_waiting, "waiting");

    // Continue anyway with valid justification (<= 500 chars) opens deferred HITL review!
    let _continued = f
        .service
        .resolve_stage_failure_with_convergence(
            ResolveStageFailureRequest {
                checkpoint_id: cp2.checkpoint_id.clone(),
                expected_state_version: cp2.state_version,
                decision: StageFailureDecision::ContinueAnyway,
                feedback: Some("  Intentional scope trade-off  ".into()),
            },
            None,
            None,
        )
        .unwrap();
    let after_continue = pulse_row(&db, "p-limit");
    assert_eq!(after_continue.status, PulsationStatus::Checkpoint);
    let hitl_purpose: String = db
        .query_row(
            "SELECT purpose FROM checkpoints WHERE pulsation_id='p-limit' AND status='waiting'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hitl_purpose, "artifact_review");
    let saved_justification: Option<String> = db
        .query_row(
            "SELECT justification FROM stage_failure_requests WHERE checkpoint_id=?1",
            [&cp2.checkpoint_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        saved_justification.as_deref(),
        Some("Intentional scope trade-off")
    );
    let overridden_status: String = db
        .query_row(
            "SELECT status FROM stage_verifier_runs
              WHERE pulsation_id='p-limit' AND stage_generation=3",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(overridden_status, "overridden");

    // Whitespace-only ContinueAnyway records NULL.
    assert_eq!(
        PersistenceService::sanitize_continue_anyway_justification(Some("   \n\t ")).unwrap(),
        None
    );

    // Revision reset check on separate pulsation:
    let _job_rev = seed_running_pulsation(
        &db,
        &f.project,
        "p-rev-reset",
        PulsationStage::Specification,
        PulsationStatus::Running,
        1,
    );
    let p_r1 = pulse_row(&db, "p-rev-reset");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(
        &tx,
        &p_r1,
        &_job_rev,
        1,
        1,
        "await_review",
    )
    .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(&db, "p-rev-reset", &v, &reject_verdict("R1", "R1", None));
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-rev-reset",
        PulsationStage::Specification,
        1,
        "2026-11-25T12:12:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    // Simulate human RequestRevision checkpoint at state_version=3 + reopened job at state_version=4.
    db.execute(
        "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,generation,purpose)
         VALUES('cp-rev','p-rev-reset','SPECIFICATION','revision_requested','2026-11-25T12:13:00Z','2026-11-25T12:13:00Z','artifact_review',1,3,1,'artifact_review')",
        [],
    )
    .unwrap();
    let job_after_rev = seed_running_pulsation(
        &db,
        &f.project,
        "p-rev-reset",
        PulsationStage::Specification,
        PulsationStatus::Running,
        4,
    );
    let p_r2 = pulse_row(&db, "p-rev-reset");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(
        &tx,
        &p_r2,
        &job_after_rev,
        2,
        2,
        "await_review",
    )
    .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(&db, "p-rev-reset", &v, &reject_verdict("R2", "R2", None));
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-rev-reset",
        PulsationStage::Specification,
        2,
        "2026-11-25T12:14:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    // Because revision reset counter to 0, R2 count is 1 <= reject_limit 1 => Planning (auto re-run), NOT checkpoint!
    assert_eq!(
        pulse_row(&db, "p-rev-reset").status,
        PulsationStatus::Planning
    );
    drop(db);
    let _ = f.workspace_path;
}

#[test]
fn advisory_verifier_never_blocks_and_position_order_caps_concurrent_per_project() {
    let f = fixture();
    enable_labs(&f, true);
    let adv = create_verifier(&f, "Code", "Advisory lint", 0, 0);
    let v1 = create_verifier(&f, "Test", "First", 1, 2);
    let v2 = create_verifier(&f, "Test", "Second", 2, 2);

    let mut db = Connection::open(f.service.database_path()).unwrap();
    // Advisory reject still advances.
    let job_adv = seed_running_pulsation(
        &db,
        &f.project,
        "p-adv",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let p_adv = pulse_row(&db, "p-adv");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(
        &tx,
        &p_adv,
        &job_adv,
        1,
        1,
        "advance:Test",
    )
    .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(
        &db,
        "p-adv",
        &adv,
        &reject_verdict("Advisory nit", "Refactor loop later", None),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-adv",
        PulsationStage::Code,
        1,
        "2026-11-25T12:00:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(pulse_row(&db, "p-adv").stage, PulsationStage::Test);

    // Publication wait & position order on Test stage.
    let job_test = seed_running_pulsation(
        &db,
        &f.project,
        "p-order",
        PulsationStage::Test,
        PulsationStatus::Running,
        1,
    );
    db.execute(
        "INSERT INTO artifacts(id,pulsation_id,name,kind,content,content_sha256,producer_stage,producer_job_id,acceptance,created_at,updated_at)
         VALUES('art-1','p-order','Test report','markdown','ok','sha1','Test',?1,'accepted','2026-11-25T12:00:00Z','2026-11-25T12:00:00Z')",
        [&job_test],
    )
    .unwrap();
    db.execute(
        "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,expected_sha256,desired_sha256,state,updated_at)
         VALUES('art-1',?1,'p-order','docs/test.md','absent','sha1','pending','2026-11-25T12:00:00Z')",
        [&f.project],
    )
    .unwrap();
    let p_order = pulse_row(&db, "p-order");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(
        &tx,
        &p_order,
        &job_test,
        1,
        1,
        "advance:Integrator",
    )
    .unwrap();
    tx.commit().unwrap();

    // Candidate publications (pre-HITL checkpoint stages) must NOT block claiming,
    // while accepted publications in pending/applying/conflict DO block claiming.
    db.execute(
        "UPDATE artifacts SET acceptance='candidate' WHERE id='art-1'",
        [],
    )
    .unwrap();
    let candidate_claim = f
        .service
        .claim_next_verifier_run()
        .unwrap()
        .expect("candidate publication must not block verifier claim");
    db.execute(
        "DELETE FROM automation_slots WHERE owner_id=?1",
        [&candidate_claim.run_id],
    )
    .unwrap();
    db.execute(
        "UPDATE stage_verifier_runs SET status='pending',started_at=NULL WHERE id=?1",
        [&candidate_claim.run_id],
    )
    .unwrap();
    db.execute(
        "UPDATE artifacts SET acceptance='accepted' WHERE id='art-1'",
        [],
    )
    .unwrap();

    // Must NOT claim while accepted publication is pending, applying, or conflict.
    assert!(f.service.claim_next_verifier_run().unwrap().is_none());
    db.execute(
        "UPDATE artifact_publications SET state='applying' WHERE artifact_id='art-1'",
        [],
    )
    .unwrap();
    assert!(f.service.claim_next_verifier_run().unwrap().is_none());
    db.execute(
        "UPDATE artifact_publications SET state='conflict' WHERE artifact_id='art-1'",
        [],
    )
    .unwrap();
    assert!(f.service.claim_next_verifier_run().unwrap().is_none());

    // Once applied, First is claimed, and Second is blocked by project cap of 1 running verifier.
    db.execute(
        "UPDATE artifact_publications SET state='applied' WHERE artifact_id='art-1'",
        [],
    )
    .unwrap();
    let claim1 = f
        .service
        .claim_next_verifier_run()
        .unwrap()
        .expect("first claim");
    assert_eq!(claim1.verifier_id, v1);
    assert!(
        f.service.claim_next_verifier_run().unwrap().is_none(),
        "only 1 running verifier allowed per project"
    );
    let _ = v2;
}

#[test]
fn crash_recovery_and_verification_error_decisions_with_receipts() {
    let f = fixture();
    enable_labs(&f, true);
    let v = create_verifier(&f, "Code", "Crash check", 0, 2);
    let mut db = Connection::open(f.service.database_path()).unwrap();
    let job = seed_running_pulsation(
        &db,
        &f.project,
        "p-err",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let p = pulse_row(&db, "p-err");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(&tx, &p, &job, 1, 1, "advance:Test")
        .unwrap();
    tx.commit().unwrap();
    let run_id: String = db
        .query_row(
            "SELECT id FROM stage_verifier_runs WHERE pulsation_id='p-err'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Crash #1 -> resets to pending with recovery_count=1.
    db.execute(
        "UPDATE stage_verifier_runs SET status='running',started_at='2026-11-25T12:00:00Z' WHERE id=?1",
        [&run_id],
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::recover_stage_verifiers_in_tx(&tx, "2026-11-25T12:01:00Z").unwrap();
    tx.commit().unwrap();
    let (st1, rc1): (String, i64) = db
        .query_row(
            "SELECT status,recovery_count FROM stage_verifier_runs WHERE id=?1",
            [&run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(st1, "pending");
    assert_eq!(rc1, 1);

    // Crash #2 -> opens verification_error checkpoint.
    db.execute(
        "UPDATE stage_verifier_runs SET status='running',started_at='2026-11-25T12:02:00Z' WHERE id=?1",
        [&run_id],
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::recover_stage_verifiers_in_tx(&tx, "2026-11-25T12:03:00Z").unwrap();
    tx.commit().unwrap();

    let cp = f
        .service
        .get_open_verification_error_checkpoint("p-err")
        .unwrap()
        .expect("verification_error checkpoint");
    assert_eq!(cp.verifier_label, "Crash check");

    // Decision 1: RetryVerifier creates attempt 2 + records ApprovalReceiptV1.
    let retried = f
        .service
        .resolve_verification_error_checkpoint(ResolveVerificationErrorRequest {
            checkpoint_id: cp.checkpoint_id.clone(),
            expected_state_version: cp.state_version,
            decision: VerificationErrorDecision::RetryVerifier,
        })
        .unwrap();
    assert_eq!(retried.pulsation.status, PulsationStatus::Running);
    let attempt2_id: String = db
        .query_row(
            "SELECT id FROM stage_verifier_runs WHERE pulsation_id='p-err' AND attempt=2",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Re-open verification_error on attempt 2, then test SkipVerifier advancing the stage.
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::open_verification_error_checkpoint_in_tx(
        &tx,
        "p-err",
        PulsationStage::Code,
        &attempt2_id,
        "timeout",
        "2026-11-25T12:04:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    let cp2 = f
        .service
        .get_open_verification_error_checkpoint("p-err")
        .unwrap()
        .expect("second verification_error checkpoint");
    let skipped = f
        .service
        .resolve_verification_error_checkpoint(ResolveVerificationErrorRequest {
            checkpoint_id: cp2.checkpoint_id,
            expected_state_version: cp2.state_version,
            decision: VerificationErrorDecision::SkipVerifier,
        })
        .unwrap();
    assert_eq!(skipped.pulsation.stage, PulsationStage::Test);
    assert_eq!(skipped.pulsation.status, PulsationStatus::Planning);

    let receipt_count: i64 = db
        .query_row(
            "SELECT count(*) FROM checkpoint_approval_receipts WHERE pulsation_id='p-err' AND purpose='verification_error'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipt_count, 2);

    // Abort on another pulsation writes terminal_reason='verification_error' and a receipt.
    let job_a = seed_running_pulsation(
        &db,
        &f.project,
        "p-abort-verr",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let p_a = pulse_row(&db, "p-abort-verr");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(
        &tx,
        &p_a,
        &job_a,
        1,
        1,
        "advance:Test",
    )
    .unwrap();
    tx.commit().unwrap();
    let run_a: String = db
        .query_row(
            "SELECT id FROM stage_verifier_runs WHERE pulsation_id='p-abort-verr' AND verifier_id=?1",
            [&v],
            |r| r.get(0),
        )
        .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::open_verification_error_checkpoint_in_tx(
        &tx,
        "p-abort-verr",
        PulsationStage::Code,
        &run_a,
        "CLI crashed",
        "2026-11-25T12:05:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    let cp_a = f
        .service
        .get_open_verification_error_checkpoint("p-abort-verr")
        .unwrap()
        .unwrap();
    let aborted = f
        .service
        .resolve_verification_error_checkpoint(ResolveVerificationErrorRequest {
            checkpoint_id: cp_a.checkpoint_id,
            expected_state_version: cp_a.state_version,
            decision: VerificationErrorDecision::Abort,
        })
        .unwrap();
    assert_eq!(aborted.pulsation.status, PulsationStatus::Failed);
    assert_eq!(
        aborted.pulsation.terminal_reason.as_deref(),
        Some("verification_error")
    );
}

#[test]
fn load_latest_rejected_findings_skips_human_revision_and_report_includes_verifications() {
    let f = fixture();
    enable_labs(&f, true);
    let v = create_verifier(&f, "Code", "Report gate", 0, 1);
    let mut db = Connection::open(f.service.database_path()).unwrap();

    // Rejection on gen 1 resolved via human RequestRevision -> gen 2 must NOT carry findings.
    let job1 = seed_running_pulsation(
        &db,
        &f.project,
        "p-s3",
        PulsationStage::Code,
        PulsationStatus::Running,
        1,
    );
    let p1 = pulse_row(&db, "p-s3");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::maybe_start_stage_verification_in_tx(&tx, &p1, &job1, 1, 1, "advance:Test")
        .unwrap();
    tx.commit().unwrap();
    mark_latest_verifier_run_verdict(
        &db,
        "p-s3",
        &v,
        &reject_verdict("Bad impl", "Missing edge case", Some("src/lib.rs")),
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    PersistenceService::settle_stage_verification_in_tx(
        &tx,
        "p-s3",
        PulsationStage::Code,
        1,
        "2026-11-25T12:00:00Z",
    )
    .unwrap();
    tx.commit().unwrap();
    // Auto re-run gen 2 gets findings.
    assert_eq!(
        load_latest_rejected_findings(&db, "p-s3", PulsationStage::Code, 2)
            .unwrap()
            .len(),
        1
    );
    // If generation 1 had a `request_revision` human failure decision, gen 2 omits them.
    db.execute(
        "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose)
         VALUES('cp-s3','p-s3','Code','revision_requested','2026-11-25T12:01:00Z','2026-11-25T12:01:00Z','stage_failure',1,2,?1,1,'stage_failure')",
        [&job1],
    )
    .unwrap();
    db.execute(
        "INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,allow_continue_anyway,revision_stage,error_code,diagnostic,decision,created_at,updated_at)
         VALUES('cp-s3','p-s3',?1,'Code','verification_rejected',1,1,1,'Code','verification_rejected','diag','request_revision','2026-11-25T12:01:00Z','2026-11-25T12:01:00Z')",
        [&job1],
    )
    .unwrap();
    assert!(
        load_latest_rejected_findings(&db, "p-s3", PulsationStage::Code, 2)
            .unwrap()
            .is_empty(),
        "human revision supersedes rejected verifier findings"
    );

    // End the pulsation and build the ADR-027 HTML report.
    db.execute(
        "UPDATE pulsations SET status='failed',terminal_reason='user_cancel' WHERE id='p-s3'",
        [],
    )
    .unwrap();
    let report = f.service.build_pulsation_report("p-s3").unwrap();
    let verifs = report
        .verifications
        .as_ref()
        .expect("verifications in report");
    assert_eq!(verifs.runs.len(), 1);
    assert_eq!(verifs.runs[0].verifier_label, "Report gate");
    assert_eq!(verifs.runs[0].findings.len(), 1);
    let html = crate::reports::render_pulsation_report(&report);
    assert!(html.contains("id=\"verifications\""));
    assert!(html.contains("Report gate"));
    assert!(html.contains("Missing edge case"));
}
