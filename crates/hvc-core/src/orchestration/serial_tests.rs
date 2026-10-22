use super::*;
use crate::agent_runtime::GeneratedArtifact;
use serde_json::json;

pub(crate) struct Fixture {
    _data: tempfile::TempDir,
    pub(crate) workspace: tempfile::TempDir,
    pub(crate) service: PersistenceService,
    pub(crate) pulse: Pulsation,
    pub(crate) job: Job,
    pub(crate) run: String,
    pub(crate) handoff: StageHandoffV1,
}

impl Fixture {
    pub(crate) fn new(stage: PulsationStage) -> Self {
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
                feature_name: "Implement a validated contract".into(),
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
    pub(crate) fn output(
        &self,
        status: &str,
        path: Option<&str>,
        content: &str,
    ) -> StageExecutionResult {
        let structured = json!({"outputLanguage":"en","summary":"Traceable result","statusUpdate":status,"notes":[],"contextUsage":[],"artifacts":[{"name":"draft","kind":"stage_output","relativePath":path,"content":content}]});
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
            context_trace: crate::agent_runtime::ContextRetrievalTrace::default(),
            artifacts: vec![GeneratedArtifact {
                name: "draft".into(),
                kind: "stage_output".into(),
                relative_path: path.map(str::to_owned),
                content: content.into(),
            }],
        }
    }
    pub(crate) fn output_artifacts(
        &self,
        status: &str,
        artifacts: Vec<GeneratedArtifact>,
    ) -> StageExecutionResult {
        let structured = json!({"outputLanguage":"en","summary":"Traceable result","statusUpdate":status,"notes":[],"contextUsage":[],"artifacts":[]});
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
            context_trace: crate::agent_runtime::ContextRetrievalTrace::default(),
            artifacts,
        }
    }
    pub(crate) fn accept(
        &self,
        output: &StageExecutionResult,
    ) -> Result<(), PersistenceServiceError> {
        let result = self.service.accept_stage_result(
            &self.job,
            &self.run,
            &self.handoff,
            output,
            self.workspace.path(),
        );
        if result.is_ok() {
            self.service
                .release_runtime_slot(&PersistenceService::job_slot_token(&self.job))?;
            self.service.unregister_job_cancellation(&self.job.id);
        }
        result
    }
    pub(crate) fn count(&self, table: &str) -> i64 {
        self.service
            .connection()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    pub(crate) fn decision(&self, decision: HitlDecision) -> ApplyHitlDecisionRequest {
        let c = self.service.get_hitl_checkpoint(&self.pulse.id).unwrap();
        ApplyHitlDecisionRequest {
            pulsation_id: self.pulse.id.clone(),
            checkpoint_id: c.checkpoint_id,
            expected_state_version: c.state_version,
            decision,
            feedback: Some("Revisar com evidência".into()),
        }
    }
    pub(crate) fn state(&self) -> Pulsation {
        PersistenceService::find_pulsation_by_id(
            &self.service.connection().unwrap(),
            &self.pulse.id,
        )
        .unwrap()
        .unwrap()
    }
}

#[test]
fn failed_outcome_never_publishes_and_parks_on_a_decision() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("failed", Some("src/result.rs"), "danger"))
        .unwrap();
    // The agent gave up, but Code has a revision target, so the pulsation waits on a
    // human decision instead of dying and forcing the whole pipeline to be re-run.
    assert_eq!(f.state().status, PulsationStatus::Checkpoint);
    assert_eq!(f.count("stage_failure_requests"), 1);
    let (class, retry, revision, target): (String, i64, i64, Option<String>) = f
        .service
        .connection()
        .unwrap()
        .query_row(
            "SELECT failure_class,allow_retry,allow_revision,revision_stage
             FROM stage_failure_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(class, "model_reported_failure");
    // A durable stage_acceptances row means the old job can never be resumed.
    assert_eq!(retry, 0);
    assert_eq!(revision, 1);
    assert_eq!(target.as_deref(), Some("Code"));

    assert_eq!(f.count("artifacts"), 0);
    assert_eq!(f.count("artifact_publications"), 0);
    assert_eq!(f.count("stage_acceptances"), 1);
    // The base is untouched: deferring mark_effects_for_review is what keeps the
    // pulsation resumable while it waits.
    let blocking: Option<String> = f
        .service
        .connection()
        .unwrap()
        .query_row(
            "SELECT blocking_reason FROM project_pipeline_state WHERE project_id=?1",
            [&f.pulse.project_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(blocking, None);
    f.service.recover_interrupted_jobs().unwrap();
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
}

#[test]
fn hitl_review_reads_complete_exact_producer_documents_independent_of_workspace() {
    let fixture = Fixture::new(PulsationStage::Contract);
    let content = format!(
        "# Complete contract\n\n{}\nLast review paragraph.\n",
        "Detailed requirement.\n".repeat(700)
    );
    fixture
        .accept(&fixture.output_artifacts(
            "checkpoint_ready",
            vec![
                GeneratedArtifact {
                    name: "Contract".into(),
                    kind: "stage_output".into(),
                    relative_path: Some("review.md".into()),
                    content: content.clone(),
                },
                GeneratedArtifact {
                    name: "Internal notes".into(),
                    kind: "stage_output".into(),
                    relative_path: None,
                    content: "# Internal notes\n\nPathless artifact content.".into(),
                },
            ],
        ))
        .unwrap();
    std::fs::write(
        fixture.workspace.path().join("review.md"),
        "Unrelated human edits",
    )
    .unwrap();
    let db = fixture.service.connection().unwrap();
    db.execute(
        "INSERT INTO artifacts(id,pulsation_id,name,kind,content,created_at,updated_at,producer_job_id,producer_stage,generation,input_revision,producer_state_version,output_position,acceptance)
         SELECT 'unrelated-generation',pulsation_id,'Wrong generation',kind,'Never show this',created_at,updated_at,producer_job_id,producer_stage,generation+1,input_revision,producer_state_version,output_position,'candidate'
         FROM artifacts WHERE name='Contract'", [],
    ).unwrap();
    let checkpoint = fixture
        .service
        .get_hitl_checkpoint(&fixture.pulse.id)
        .unwrap();
    let documents = fixture.service.read_hitl_review(&checkpoint).unwrap();
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].name, "Contract");
    assert_eq!(documents[0].content, content);
    assert_eq!(documents[1].name, "Internal notes");
    assert_eq!(documents[1].relative_path, None);
    assert!(documents[1].content.contains("Pathless artifact content."));
}

#[test]
fn hitl_review_rejects_stale_cross_pulsation_and_resolved_checkpoints() {
    let fixture = Fixture::new(PulsationStage::Specification);
    fixture
        .accept(&fixture.output(
            "checkpoint_ready",
            None,
            "# Specification\n\nComplete proposal.",
        ))
        .unwrap();
    let checkpoint = fixture
        .service
        .get_hitl_checkpoint(&fixture.pulse.id)
        .unwrap();
    assert_eq!(
        fixture.service.read_hitl_review(&checkpoint).unwrap().len(),
        1
    );
    for altered in [
        HitlCheckpoint {
            pulsation_id: "different-pulse".into(),
            ..checkpoint.clone()
        },
        HitlCheckpoint {
            checkpoint_id: "different-checkpoint".into(),
            ..checkpoint.clone()
        },
        HitlCheckpoint {
            state_version: checkpoint.state_version + 1,
            ..checkpoint.clone()
        },
    ] {
        assert!(fixture.service.read_hitl_review(&altered).is_err());
    }
    fixture
        .service
        .apply_hitl_decision(fixture.decision(HitlDecision::RequestRevision))
        .unwrap();
    assert!(fixture.service.read_hitl_review(&checkpoint).is_err());
}

#[test]
fn hitl_review_never_reconstructs_missing_legacy_content() {
    let fixture = Fixture::new(PulsationStage::Contract);
    fixture
        .accept(&fixture.output("checkpoint_ready", None, "# Contract\n\nComplete proposal."))
        .unwrap();
    let checkpoint = fixture
        .service
        .get_hitl_checkpoint(&fixture.pulse.id)
        .unwrap();
    fixture
        .service
        .connection()
        .unwrap()
        .execute(
            "UPDATE artifacts SET content=NULL WHERE pulsation_id=?1",
            [&fixture.pulse.id],
        )
        .unwrap();
    assert!(fixture.service.read_hitl_review(&checkpoint).is_err());
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
fn accepted_stage_persists_context_pool_delivery_and_citation_atomically() {
    let f = Fixture::new(PulsationStage::Contract);
    let mut output = f.output("checkpoint_ready", None, "contract");
    output.context_trace = crate::agent_runtime::ContextRetrievalTrace {
        requested_strategy: "yaml_graph_rag".into(),
        strategy_used: "yaml_graph_rag".into(),
        adaptive_mode: "on".into(),
        expansion_depth: Some(2),
        fallback_used: false,
        max_similarity: Some(0.88),
        duration_ms: 42,
        items: vec![crate::agent_runtime::ContextRetrievalItemTrace {
            relative_path: "knowledge/obsidian/features/root.md".into(),
            item_kind: "note".into(),
            content_sha256: Some("a".repeat(64)),
            rank: 1,
            graph_score: Some(100),
            semantic_similarity: Some(0.88),
            final_score: Some(194),
            delivered: true,
            delivered_chars: 512,
            cited: true,
            citation_reason: Some("Contrato normativo".into()),
        }],
    };
    f.accept(&output).unwrap();
    let db = f.service.connection().unwrap();
    let totals: (i64, i64, i64) = db
        .query_row(
            "SELECT considered_total,delivered_total,cited_total FROM context_retrieval_runs WHERE agent_run_id=?1",
            [&f.run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(totals, (1, 1, 1));
    assert_eq!(
        db.query_row(
            "SELECT citation_reason FROM context_retrieval_items",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "Contrato normativo"
    );
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

#[test]
fn non_english_agent_artifact_is_rejected_before_persistence() {
    let fixture = Fixture::new(PulsationStage::Intake);
    let error = fixture
        .accept(&fixture.output(
            "advance",
            Some("reports/result.md"),
            "Este arquivo foi gerado pelo agente",
        ))
        .unwrap_err();

    assert!(error.to_string().contains("agent artifact must be English"));
    assert_eq!(fixture.count("artifacts"), 0);
    assert_eq!(fixture.count("agent_run_outputs"), 0);
}

#[test]
fn invalid_language_metadata_or_structured_text_is_rejected_before_persistence() {
    for mutation in ["missing_marker", "non_english_summary"] {
        let fixture = Fixture::new(PulsationStage::Intake);
        let mut output = fixture.output("advance", None, "English artifact content");
        match mutation {
            "missing_marker" => {
                output
                    .structured_output
                    .as_object_mut()
                    .unwrap()
                    .remove("outputLanguage");
            }
            "non_english_summary" => {
                output.structured_output["summary"] = json!("Relatorio gerado pelo agente");
            }
            _ => unreachable!(),
        }
        output.raw_output = output.structured_output.to_string();

        let error = fixture.accept(&output).unwrap_err();
        assert!(error.to_string().contains("must"));
        assert_eq!(fixture.count("artifacts"), 0);
        assert_eq!(fixture.count("agent_run_outputs"), 0);
    }
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

/// Field incident, 2026-09-09 (pulsation `250892e3-...`): the Test stage's
/// second attempt passed all 5 model-output validations but was then rejected
/// by `validate_generated_vault_note` for a missing `source-files` frontmatter
/// key - and, because that check used `invalid()` (host-side, never
/// retryable), the job died there with no third attempt and no raw output
/// persisted anywhere. All 4 publication checks in this loop only inspect the
/// artifacts of the response being accepted right now, never other stored
/// artifacts, so they belong in the same `ModelOutputInvalid` retry gate as
/// the 5 validations that precede them.
#[test]
fn oversized_artifact_content_is_a_retryable_model_output_error() {
    let f = Fixture::new(PulsationStage::Code);
    let output = f.output_artifacts(
        "advance",
        vec![GeneratedArtifact {
            name: "draft".into(),
            kind: "stage_output".into(),
            relative_path: Some("src/big.rs".into()),
            content: "a".repeat(1024 * 1024 + 1),
        }],
    );
    assert!(matches!(
        f.accept(&output),
        Err(PersistenceServiceError::ModelOutputInvalid(_))
    ));
}

#[test]
fn a_reserved_artifact_kind_is_a_retryable_model_output_error() {
    let f = Fixture::new(PulsationStage::Code);
    let output = f.output_artifacts(
        "advance",
        vec![GeneratedArtifact {
            name: "draft".into(),
            kind: "code_context".into(),
            relative_path: Some("src/generated.rs".into()),
            content: "fn generated() {}\n".into(),
        }],
    );
    assert!(matches!(
        f.accept(&output),
        Err(PersistenceServiceError::ModelOutputInvalid(_))
    ));
}

#[test]
fn a_vault_note_missing_source_files_is_a_retryable_model_output_error() {
    let f = Fixture::new(PulsationStage::Test);
    let output = f.output_artifacts(
        "advance",
        vec![GeneratedArtifact {
            name: "T1_1".into(),
            kind: "test".into(),
            relative_path: Some("knowledge/obsidian/tests/T1_1-architecture.md".into()),
            content: "# Architecture decisions documented\n".into(),
        }],
    );
    let error = f
        .accept(&output)
        .expect_err("missing source-files must fail");
    assert!(
        matches!(error, PersistenceServiceError::ModelOutputInvalid(ref message) if message.contains("source-files"))
    );
}

#[test]
fn duplicate_artifact_destinations_are_a_retryable_model_output_error() {
    let f = Fixture::new(PulsationStage::Code);
    let output = f.output_artifacts(
        "advance",
        vec![
            GeneratedArtifact {
                name: "draft-a".into(),
                kind: "stage_output".into(),
                relative_path: Some("src/dup.rs".into()),
                content: "fn a() {}\n".into(),
            },
            GeneratedArtifact {
                name: "draft-b".into(),
                kind: "stage_output".into(),
                relative_path: Some("SRC/DUP.rs".into()),
                content: "fn b() {}\n".into(),
            },
        ],
    );
    assert!(matches!(
        f.accept(&output),
        Err(PersistenceServiceError::ModelOutputInvalid(_))
    ));
}

#[test]
fn sanitization_rejects_placeholder_publication_without_writing_a_candidate() {
    let f = Fixture::new(PulsationStage::Code);
    assert!(
        f.accept(&f.output(
            "advance",
            Some("code.rs"),
            "const KEY: &str = \"<REDACTED_SECRET:KNOWN_API_TOKEN_1>\";\n"
        ))
        .is_err()
    );
    assert!(!f.workspace.path().join("code.rs").exists());
    assert_eq!(f.count("artifacts"), 0);
}

#[test]
fn sanitization_publication_guard_survives_recovery_and_rejects_tampering() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "pub fn safe() {}\n"))
        .unwrap();
    let db = f.service.connection().unwrap();
    assert_eq!(f.count("secret_publication_guards"), 1);
    db.execute(
        "UPDATE secret_publication_guards SET desired_sha256=?1",
        ["0".repeat(64)],
    )
    .unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert!(!f.workspace.path().join("code.rs").exists());
    let state: String = db
        .query_row("SELECT state FROM artifact_publications", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(state, "conflict");
}

#[test]
fn sanitization_policy_change_requires_new_publication_preparation() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "pub fn safe() {}\n"))
        .unwrap();
    fs::write(
        f.workspace.path().join("fixture.rs"),
        "ghp_syntheticabcdefghijklmnop",
    )
    .unwrap();
    let store = crate::sanitization::SanitizationStore::new(f.service.database_path());
    let preview = store
        .preview_file(&f.pulse.project_id, "fixture.rs")
        .unwrap();
    let span = &preview.spans[0];
    store
        .create_exception(&crate::sanitization::CreateSecretRedactionException {
            project_id: f.pulse.project_id.clone(),
            relative_path: "fixture.rs".into(),
            detector_id: span.detector_id.clone(),
            detector_version: span.detector_version,
            original_file_sha256: preview.original_file_sha256,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            reason: "A synthetic fixture approved locally".into(),
        })
        .unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert!(!f.workspace.path().join("code.rs").exists());
    assert_eq!(
        f.service
            .connection()
            .unwrap()
            .query_row("SELECT state FROM artifact_publications", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "conflict"
    );
}

#[test]
fn sanitization_acceptance_checks_the_frozen_policy_inside_its_transaction() {
    let f = Fixture::new(PulsationStage::Code);
    let store = crate::sanitization::SanitizationStore::new(f.service.database_path());
    let frozen = store
        .policy_for_project(&f.pulse.project_id)
        .unwrap()
        .identity;
    fs::write(
        f.workspace.path().join("fixture.rs"),
        "ghp_syntheticabcdefghijklmnop",
    )
    .unwrap();
    let preview = store
        .preview_file(&f.pulse.project_id, "fixture.rs")
        .unwrap();
    let span = &preview.spans[0];
    store
        .create_exception(&crate::sanitization::CreateSecretRedactionException {
            project_id: f.pulse.project_id.clone(),
            relative_path: "fixture.rs".into(),
            detector_id: span.detector_id.clone(),
            detector_version: span.detector_version,
            original_file_sha256: preview.original_file_sha256,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            reason: "A synthetic fixture approved locally".into(),
        })
        .unwrap();
    let mut db = f.service.connection().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let current = PersistenceService::find_pulsation_by_id(&tx, &f.pulse.id)
        .unwrap()
        .unwrap();
    let outcome = f
        .output("advance", Some("code.rs"), "pub fn safe() {}\n")
        .outcome()
        .unwrap();
    let error = PersistenceService::accept_stage_outcome_in_tx(
        &tx,
        &f.job,
        &f.run,
        &f.handoff,
        &current,
        &outcome,
        &AcceptanceProvenance {
            sanitization_policy_identity: Some(&frozen),
            ..Default::default()
        },
        f.workspace.path(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("policy changed before acceptance")
    );
    tx.rollback().unwrap();
    assert_eq!(f.count("stage_acceptances"), 0);
    assert!(!f.workspace.path().join("code.rs").exists());
}

#[test]
fn sanitization_legacy_uncertain_write_with_placeholder_is_not_acknowledged() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "pub fn safe() {}\n"))
        .unwrap();
    let content = "const KEY: &str = \"<REDACTED_SECRET:KNOWN_API_TOKEN_1>\";\n";
    let digest = ContentSha256::of(content.as_bytes());
    let db = f.service.connection().unwrap();
    db.execute(
        "UPDATE artifacts SET content=?1,content_sha256=?2",
        params![content, digest.as_str()],
    )
    .unwrap();
    db.execute(
        "UPDATE artifact_publications SET desired_sha256=?1",
        [digest.as_str()],
    )
    .unwrap();
    db.execute("DELETE FROM secret_publication_guards", [])
        .unwrap();
    fs::write(f.workspace.path().join("code.rs"), content).unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("code.rs")).unwrap(),
        content
    );
    assert_eq!(f.count("secret_publication_guards"), 0);
    assert_eq!(
        db.query_row("SELECT state FROM artifact_publications", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "conflict"
    );
}
