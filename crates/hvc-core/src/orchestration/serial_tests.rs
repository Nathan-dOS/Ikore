use super::*;
use crate::agent_runtime::GeneratedArtifact;
use serde_json::json;

struct Fixture {
    _data: tempfile::TempDir,
    workspace: tempfile::TempDir,
    service: PersistenceService,
    pulse: Pulsation,
    job: Job,
    run: String,
    handoff: StageHandoffV1,
}

impl Fixture {
    fn new(stage: PulsationStage) -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = PersistenceService::initialize(data.path()).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project;
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.id,
                feature_name: "Implementar contrato validado".into(),
            })
            .unwrap()
            .pulsation;
        let db = service.connection().unwrap();
        db.execute(
            "UPDATE pulsations SET stage=?2 WHERE id=?1",
            params![pulse.id, stage.as_db_value()],
        )
        .unwrap();
        db.execute(
            "UPDATE jobs SET kind=?2 WHERE pulsation_id=?1",
            params![pulse.id, format!("run_stage::{}", stage.as_db_value())],
        )
        .unwrap();
        let (job, _) = service.claim_next_pending_job().unwrap().unwrap();
        let tx = db.unchecked_transaction().unwrap();
        let current = PersistenceService::find_pulsation_by_id(&tx, &pulse.id)
            .unwrap()
            .unwrap();
        PersistenceService::transition_pulsation(
            &tx,
            &current,
            PulsationStatus::Running,
            stage,
            "test_start",
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        let run = service
            .start_agent_run_for_job(&job, &AgentRuntimeConfig::default())
            .unwrap();
        let fence = PersistenceService::execution_fence(&db, &job.id).unwrap();
        let handoff = service.load_stage_handoff(&fence, stage, 16000).unwrap();
        Self {
            _data: data,
            workspace,
            service,
            pulse,
            job,
            run,
            handoff,
        }
    }
    fn output(&self, status: &str, path: Option<&str>, content: &str) -> StageExecutionResult {
        let structured = json!({"summary":"Resultado rastreável","statusUpdate":status,"notes":[],"artifacts":[{"name":"draft","kind":"stage_output","relativePath":path,"content":content}]});
        StageExecutionResult {
            provider_used: "fixture".into(),
            model_used: "fixture".into(),
            route_id: None,
            connection_id: None,
            llm_execution_id: None,
            input_tokens: None,
            output_tokens: None,
            estimated_cost_usd: None,
            request_payload: json!({}),
            response_payload: json!({}),
            raw_output: structured.to_string(),
            structured_output: structured,
            schema_json: json!({}),
            tool_trace: json!([]),
            artifacts: vec![GeneratedArtifact {
                name: "draft".into(),
                kind: "stage_output".into(),
                relative_path: path.map(str::to_owned),
                content: content.into(),
            }],
        }
    }
    fn accept(&self, output: &StageExecutionResult) -> Result<(), PersistenceServiceError> {
        self.service.accept_stage_result(
            &self.job,
            &self.run,
            &self.handoff,
            output,
            self.workspace.path(),
        )
    }
    fn count(&self, table: &str) -> i64 {
        self.service
            .connection()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn decision(&self, decision: HitlDecision) -> ApplyHitlDecisionRequest {
        let c = self.service.get_hitl_checkpoint(&self.pulse.id).unwrap();
        ApplyHitlDecisionRequest {
            pulsation_id: self.pulse.id.clone(),
            checkpoint_id: c.checkpoint_id,
            expected_state_version: c.state_version,
            decision,
            feedback: Some("Revisar com evidência".into()),
        }
    }
    fn state(&self) -> Pulsation {
        PersistenceService::find_pulsation_by_id(
            &self.service.connection().unwrap(),
            &self.pulse.id,
        )
        .unwrap()
        .unwrap()
    }
}

#[test]
fn failed_outcome_never_publishes_or_advances() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("failed", Some("src/result.rs"), "danger"))
        .unwrap();
    assert_eq!(f.state().status, PulsationStatus::Failed);
    assert_eq!(f.count("artifacts"), 0);
    assert_eq!(f.count("artifact_publications"), 0);
    assert_eq!(f.count("stage_acceptances"), 1);
    f.service.recover_interrupted_jobs().unwrap();
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
}

#[test]
fn stale_revision_generation_or_version_rolls_back_all_acceptance_effects() {
    for column in ["input_revision", "state_version", "generation"] {
        let f = Fixture::new(PulsationStage::Specification);
        let (table, id) = if column == "generation" {
            ("jobs", &f.job.id)
        } else {
            ("pulsations", &f.pulse.id)
        };
        f.service
            .connection()
            .unwrap()
            .execute(
                &format!("UPDATE {table} SET {column}={column}+1 WHERE id=?1"),
                [id],
            )
            .unwrap();
        assert!(
            f.accept(&f.output("advance", Some("new.md"), "stale"))
                .is_err()
        );
        for table in [
            "artifacts",
            "artifact_publications",
            "stage_acceptances",
            "agent_run_outputs",
            "checkpoints",
        ] {
            assert_eq!(f.count(table), 0, "{column}/{table}");
        }
        assert!(!f.workspace.path().join("new.md").exists());
    }
}

#[test]
fn failure_inside_acceptance_rolls_back_output_and_checkpoint() {
    let f = Fixture::new(PulsationStage::Specification);
    let db = f.service.connection().unwrap();
    db.execute_batch("CREATE TRIGGER injected_crash BEFORE INSERT ON checkpoints BEGIN SELECT RAISE(ABORT,'crash before acceptance commit'); END;").unwrap();
    let output = f.output("advance", None, "exact\r\n");
    assert!(f.accept(&output).is_err());
    for table in [
        "artifacts",
        "stage_acceptances",
        "agent_run_outputs",
        "checkpoints",
    ] {
        assert_eq!(f.count(table), 0);
    }
    db.execute_batch("DROP TRIGGER injected_crash;").unwrap();
    f.accept(&output).unwrap();
    assert_eq!(f.count("checkpoints"), 1);
    assert_eq!(f.count("stage_acceptances"), 1);
    assert!(f.accept(&output).is_err());
    assert_eq!(f.count("artifacts"), 1);
}

#[test]
fn crash_after_acceptance_recovers_publication_without_reexecuting_stage() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("src/new.rs"), "pub fn new() {}\r\n"))
        .unwrap();
    assert!(!f.workspace.path().join("src/new.rs").exists());
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
    f.service.recover_interrupted_jobs().unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("src/new.rs")).unwrap(),
        "pub fn new() {}\r\n"
    );
    assert_eq!(f.count("agent_runs"), 1);
    assert_eq!(f.count("stage_acceptances"), 1);
    assert_eq!(f.count("artifacts"), 1);
    assert_eq!(
        f.service.claim_next_pending_job().unwrap().unwrap().0.kind,
        "run_stage::Test"
    );
}

#[test]
fn crash_after_replace_before_ack_replays_desired_hash() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "exact bytes\n"))
        .unwrap();
    let db = f.service.connection().unwrap();
    // Abort the ACK transaction after the REAL publisher has performed the replace.
    db.execute_batch("CREATE TRIGGER fail_ack BEFORE UPDATE OF state ON artifact_publications WHEN NEW.state='applied' BEGIN SELECT RAISE(ABORT,'crash after write before ACK'); END;").unwrap();
    assert!(f.service.reconcile_publications().is_err());
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("code.rs")).unwrap(),
        "exact bytes\n"
    );
    assert_eq!(
        db.query_row("SELECT state FROM artifact_publications", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "applying"
    );
    db.execute_batch("DROP TRIGGER fail_ack;").unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        db.query_row("SELECT state FROM artifact_publications", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "applied"
    );
    assert_eq!(f.count("artifacts"), 1);
}

#[test]
fn human_change_after_uncertain_write_is_preserved_and_review_recorded() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "generated"))
        .unwrap();
    fs::write(f.workspace.path().join("code.rs"), "human edit").unwrap();
    let _ = f.service.reconcile_publications();
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("code.rs")).unwrap(),
        "human edit"
    );
    assert_eq!(f.state().status, PulsationStatus::Failed);
    assert_eq!(
        f.service
            .connection()
            .unwrap()
            .query_row("SELECT base_state FROM project_pipeline_state", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "needs_review"
    );
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
}

#[test]
fn unapproved_candidates_wait_and_pathless_content_reaches_handoff_exactly() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "  approved specification\r\n"))
        .unwrap();
    let id: String = f
        .service
        .connection()
        .unwrap()
        .query_row("SELECT id FROM artifacts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        f.service.read_artifact_content(&f.pulse.id, &id).unwrap(),
        "  approved specification\r\n"
    );
    assert!(f.service.read_artifact_content("other-pulse", &id).is_err());
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
    let old = f.decision(HitlDecision::Approve);
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    assert!(f.service.apply_hitl_decision(old).is_err());
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    let fence =
        PersistenceService::execution_fence(&f.service.connection().unwrap(), &job.id).unwrap();
    let h = f
        .service
        .load_stage_handoff(&fence, PulsationStage::Contract, 1000)
        .unwrap();
    assert_eq!(h.artifacts.len(), 1);
    assert_eq!(h.artifacts[0].content, "  approved specification\r\n");
    assert!(
        f.service
            .load_stage_handoff(&fence, PulsationStage::Contract, 2)
            .is_err()
    );
}

#[test]
fn artifact_review_does_not_write_before_approval() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("checkpoint_ready", Some("spec.md"), "approved later"))
        .unwrap();
    f.service.reconcile_publications().unwrap();
    assert!(!f.workspace.path().join("spec.md").exists());
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("spec.md")).unwrap(),
        "approved later"
    );
}

#[test]
fn revision_goes_to_correct_stage_and_old_click_cannot_approve_new_checkpoint() {
    for (stage, target) in [
        (PulsationStage::Specification, PulsationStage::Specification),
        (PulsationStage::Contract, PulsationStage::Contract),
        (PulsationStage::Test, PulsationStage::Code),
        (PulsationStage::Integrator, PulsationStage::Code),
    ] {
        let f = Fixture::new(stage);
        f.accept(&f.output("advance", None, "revise")).unwrap();
        let stale = f.decision(HitlDecision::Approve);
        f.service
            .apply_hitl_decision(f.decision(HitlDecision::RequestRevision))
            .unwrap();
        assert_eq!(f.state().stage, target);
        assert_eq!(
            f.service
                .connection()
                .unwrap()
                .query_row("SELECT acceptance FROM artifacts", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "superseded"
        );
        // Simulate the next result at the same stage: old checkpoint ID still cannot approve it.
        let db = f.service.connection().unwrap();
        db.execute(
            "UPDATE pulsations SET status='checkpoint' WHERE id=?1",
            [&f.pulse.id],
        )
        .unwrap();
        PersistenceService::open_checkpoint(&db, &f.pulse.id, target).unwrap();
        assert!(f.service.apply_hitl_decision(stale).is_err());
    }
}

#[test]
fn user_cancel_is_terminal_for_running_pending_and_accepted_publications() {
    for accepted in [false, true] {
        let f = Fixture::new(PulsationStage::Code);
        if accepted {
            f.accept(&f.output("advance", Some("cancel.rs"), "never write"))
                .unwrap();
        }
        f.service
            .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: f.pulse.id.clone(),
            })
            .unwrap();
        f.service.recover_interrupted_jobs().unwrap();
        assert!(f.service.claim_next_pending_job().unwrap().is_none());
        assert_eq!(f.state().status, PulsationStatus::Failed);
        assert!(!f.workspace.path().join("cancel.rs").exists());
        assert!(
            f.accept(&f.output("advance", Some("late.rs"), "late"))
                .is_err()
        );
    }
}

#[test]
fn shutdown_requeues_only_unaccepted_current_execution_and_fences_late_result() {
    let f = Fixture::new(PulsationStage::Code);
    f.service.cancel_all_active_jobs();
    f.service.recover_interrupted_jobs().unwrap();
    assert!(
        f.accept(&f.output("advance", None, "late shutdown result"))
            .is_err()
    );
    assert!(f.service.claim_next_pending_job().unwrap().is_some());
    assert_eq!(f.count("stage_acceptances"), 0);
}

#[test]
fn integrator_done_waits_for_publication_ack() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", Some("integration.md"), "integrated"))
        .unwrap();
    let db = f.service.connection().unwrap();
    db.execute_batch("CREATE TRIGGER fail_final_ack BEFORE UPDATE OF state ON artifact_publications WHEN NEW.state='applied' BEGIN SELECT RAISE(ABORT,'crash'); END;").unwrap();
    assert!(
        f.service
            .apply_hitl_decision(f.decision(HitlDecision::Approve))
            .is_err()
    );
    assert_ne!(f.state().status, PulsationStatus::Done);
    db.execute_batch("DROP TRIGGER fail_final_ack;").unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(f.state().status, PulsationStatus::Done);
}

#[test]
fn publisher_rejects_protected_and_escaping_paths_without_side_effects() {
    let f = Fixture::new(PulsationStage::Code);
    for path in [
        "../escape.md",
        ".env",
        ".env.example",
        ".git/config",
        "CON.txt",
        "src/a.rs:stream",
        "/absolute",
    ] {
        assert!(
            f.accept(&f.output("advance", Some(path), "unsafe"))
                .is_err(),
            "{path}"
        );
    }
    assert_eq!(f.count("artifacts"), 0);
    assert_eq!(f.count("stage_acceptances"), 0);
}

#[test]
fn expected_hash_replacement_preserves_exact_bytes_and_detects_human_change() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("existing.md");
    fs::write(&path, "old").unwrap();
    let expected = ExpectedFileContent::Present(ContentSha256::of(b"old"));
    publication::publish(root.path(), "existing.md", &expected, "new\r\n").unwrap();
    publication::publish(root.path(), "existing.md", &expected, "new\r\n").unwrap();
    fs::write(&path, "human").unwrap();
    assert!(publication::publish(root.path(), "existing.md", &expected, "new\r\n").is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "human");
}

#[test]
fn corruption_or_superseded_producer_cannot_publish() {
    for corrupt_hash in [true, false] {
        let f = Fixture::new(PulsationStage::Code);
        f.accept(&f.output("advance", Some("new.rs"), "valid"))
            .unwrap();
        let db = f.service.connection().unwrap();
        if corrupt_hash {
            db.execute("UPDATE artifacts SET content='tampered'", [])
                .unwrap();
        } else {
            db.execute(
                "UPDATE jobs SET generation=generation+1 WHERE id=?1",
                [&f.job.id],
            )
            .unwrap();
        }
        f.service.recover_interrupted_jobs().unwrap();
        assert!(!f.workspace.path().join("new.rs").exists());
    }
}

#[test]
fn cancellation_after_uncertain_write_keeps_effect_and_never_replays() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "published unit"))
        .unwrap();
    let db = f.service.connection().unwrap();
    db.execute_batch("CREATE TRIGGER fail_ack BEFORE UPDATE OF state ON artifact_publications WHEN NEW.state='applied' BEGIN SELECT RAISE(ABORT,'crash'); END;").unwrap();
    assert!(f.service.reconcile_publications().is_err());
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    db.execute_batch("DROP TRIGGER fail_ack;").unwrap();
    fs::write(f.workspace.path().join("code.rs"), "human after cancel").unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("code.rs")).unwrap(),
        "human after cancel"
    );
    assert_eq!(
        db.query_row("SELECT base_state FROM project_pipeline_state", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "needs_review"
    );
    assert_eq!(
        db.query_row("SELECT state FROM artifact_publications", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "cancelled"
    );
}

#[test]
fn publication_bases_survive_revision_without_promoting_superseded_content() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("src/code.rs"), "first version"))
        .unwrap();
    f.service.reconcile_publications().unwrap();
    let db = f.service.connection().unwrap();
    db.execute("UPDATE artifacts SET acceptance='superseded'", [])
        .unwrap();
    db.execute(
        "UPDATE pulsations SET stage='Code',status='planning' WHERE id=?1",
        [&f.pulse.id],
    )
    .unwrap();
    db.execute(
        "UPDATE jobs SET kind='run_stage::Code' WHERE status='pending'",
        [],
    )
    .unwrap();
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    let fence = PersistenceService::execution_fence(&db, &job.id).unwrap();
    let handoff = f
        .service
        .load_stage_handoff(&fence, PulsationStage::Code, 100)
        .unwrap();
    assert!(handoff.artifacts.is_empty());
    assert_eq!(handoff.publication_bases.len(), 1);
    let expected =
        ExpectedFileContent::Present(handoff.publication_bases[0].content_sha256.clone());
    publication::publish(
        f.workspace.path(),
        "src/code.rs",
        &expected,
        "second version",
    )
    .unwrap();
}

#[cfg(windows)]
#[test]
fn windows_junction_cannot_escape_publication_root() {
    use std::os::windows::process::CommandExt;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = root.path().join("junction");
    let result = std::process::Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(outside.path())
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        publication::publish(
            root.path(),
            "junction/escape.md",
            &ExpectedFileContent::Absent,
            "unsafe"
        )
        .is_err()
    );
    assert!(!outside.path().join("escape.md").exists());
    fs::remove_dir(link).unwrap();
}
