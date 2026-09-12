use super::*;
use crate::orchestration::{BaseState, ConfirmBaseReconciliationRequest};
use crate::persistence_service::serial::tests::Fixture;
use crate::persistence_service::{ResolveStageFailureRequest, StageFailureDecision};

fn queue(f: &Fixture, project: &str, stage: PulsationStage) -> Pulsation {
    let p = f
        .service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.into(),
            feature_name: "Próxima tarefa".into(),
        })
        .unwrap()
        .pulsation;
    let db = f.service.connection().unwrap();
    db.execute(
        "UPDATE pulsations SET stage=?2,created_at='2026-08-28T00:00:00Z' WHERE id=?1",
        params![p.id, stage.as_db_value()],
    )
    .unwrap();
    db.execute(
        "UPDATE jobs SET kind=?2 WHERE pulsation_id=?1",
        params![p.id, format!("run_stage::{}", stage.as_db_value())],
    )
    .unwrap();
    p
}

fn state(f: &Fixture) -> crate::orchestration::ProjectCoordinationSnapshot {
    f.service
        .get_project_coordination(&f.pulse.project_id)
        .unwrap()
}

fn confirmation(f: &Fixture, resume: Option<String>) -> ConfirmBaseReconciliationRequest {
    let s = state(f);
    ConfirmBaseReconciliationRequest {
        project_id: f.pulse.project_id.clone(),
        expected_base_epoch: s.pipeline.base_epoch,
        expected_pipeline_state_version: s.pipeline.version,
        explanation: "Conferi os arquivos e adoto esta base manualmente.".into(),
        resume_pulsation_id: resume,
        expected_review_token: s.review_token,
    }
}

fn other_project(f: &Fixture) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let id = f
        .service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: dir.path().to_string_lossy().into_owned(),
            preferred_name: None,
        })
        .unwrap()
        .project
        .id;
    (dir, id)
}

#[test]
fn fifo_abc_equal_timestamps_owner_holds_hitl_without_slot_or_retry() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "spec")).unwrap();
    let b = queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    let c = queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE pulsations SET created_at='2026-08-28T00:00:00Z'",
            [],
        )
        .unwrap();
    for _ in 0..4 {
        assert!(f.service.claim_next_pending_job().unwrap().is_none());
    }
    assert_eq!(
        state(&f).pipeline.active_pulsation_id.as_deref(),
        Some(f.pulse.id.as_str())
    );
    assert_eq!(f.count("runtime_slots"), 0);
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT SUM(attempts) FROM jobs WHERE pulsation_id<>?1",
            [&f.pulse.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        state(&f)
            .queue
            .iter()
            .map(|p| p.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(job.pulsation_id, Some(b.id));
    assert_ne!(job.pulsation_id, Some(c.id));
}

#[test]
fn a_at_hitl_does_not_block_different_project() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "spec")).unwrap();
    queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    let (_root, project) = other_project(&f);
    let other = queue(&f, &project, PulsationStage::Specification);
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(job.pulsation_id, Some(other.id.clone()));
    assert_eq!(
        state(&f).pipeline.active_pulsation_id,
        Some(f.pulse.id.clone())
    );
    assert_eq!(
        f.service
            .get_project_coordination(&project)
            .unwrap()
            .pipeline
            .active_pulsation_id,
        Some(other.id)
    );
}

#[test]
fn concurrent_claims_share_one_global_slot_and_cancel_keeps_live_reservation() {
    let f = Fixture::new(PulsationStage::Specification);
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    let (_root, project) = other_project(&f);
    queue(&f, &project, PulsationStage::Specification);
    assert!(
        f.service.claim_next_pending_job().unwrap().is_none(),
        "cancel does not detach a live executor"
    );
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&f.job))
        .unwrap();
    let barrier = std::sync::Barrier::new(3);
    let outcomes = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            handles.push(scope.spawn(|| {
                barrier.wait();
                f.service.claim_next_pending_job().unwrap().is_some()
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(outcomes.iter().filter(|v| **v).count(), 1);
    assert_eq!(f.count("runtime_slots"), 1);
}

#[test]
fn approval_epoch_release_and_pending_learning_intent_are_atomic_idempotent() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", Some("integration.md"), "final"))
        .unwrap();
    queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    let db = f.service.connection().unwrap();
    db.execute_batch("CREATE TRIGGER fail_done BEFORE UPDATE OF status ON pulsations WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'crash before final transaction'); END;").unwrap();
    assert!(
        f.service
            .apply_hitl_decision(f.decision(HitlDecision::Approve))
            .is_err()
    );
    assert_eq!(state(&f).pipeline.base_epoch, 0);
    assert_eq!(
        state(&f).pipeline.active_pulsation_id,
        Some(f.pulse.id.clone())
    );
    assert_eq!(f.count("integration_learning_intents"), 0);
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
    db.execute_batch("DROP TRIGGER fail_done").unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(f.state().status, PulsationStatus::Done);
    assert_eq!(state(&f).pipeline.base_epoch, 1);
    assert!(state(&f).pipeline.active_pulsation_id.is_none());
    assert_eq!(f.count("integration_learning_intents"), 1);
    assert_eq!(
        db.query_row("SELECT state FROM integration_learning_intents", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "pending"
    );
}

#[test]
fn learning_disabled_or_legacy_never_creates_intent() {
    for legacy in [false, true] {
        let f = Fixture::new(PulsationStage::Integrator);
        let db = f.service.connection().unwrap();
        if legacy {
            db.execute("UPDATE pulsations SET pipeline_version=1", [])
                .unwrap();
        } else {
            db.execute("UPDATE runtime_settings SET value='false' WHERE key='orchestration.learning_enabled'",[]).unwrap();
        }
        f.accept(&f.output("advance", None, "no-op")).unwrap();
        f.service
            .apply_hitl_decision(f.decision(HitlDecision::Approve))
            .unwrap();
        assert_eq!(f.state().status, PulsationStatus::Done);
        assert_eq!(f.count("integration_learning_intents"), 0);
    }
}

#[test]
fn duplicate_concurrent_approval_cannot_increment_epoch_twice() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", None, "no-op")).unwrap();
    let a = f.decision(HitlDecision::Approve);
    let b = f.decision(HitlDecision::Approve);
    let barrier = std::sync::Barrier::new(3);
    let outcomes = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            f.service.apply_hitl_decision(a).is_ok()
        });
        let second = scope.spawn(|| {
            barrier.wait();
            f.service.apply_hitl_decision(b).is_ok()
        });
        barrier.wait();
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(outcomes.iter().filter(|v| **v).count(), 1);
    f.service.reconcile_publications().unwrap();
    assert_eq!(state(&f).pipeline.base_epoch, 1);
}

#[test]
fn optional_background_reservation_cannot_block_integration_or_next_specification() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", None, "no-op")).unwrap();
    let b = queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    let db = f.service.connection().unwrap();
    assert!(
        PersistenceService::reserve_slot(
            &db,
            JobClass::Background,
            "simulated-preparation",
            Some(&f.pulse.project_id),
            None
        )
        .unwrap()
    );
    let _background = f.service.slot_guard("simulated-preparation".into());
    assert!(
        !PersistenceService::reserve_slot(&db, JobClass::Background, "second", None, None).unwrap()
    );
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    assert_eq!(f.state().status, PulsationStatus::Done);
    assert_eq!(
        f.service
            .claim_next_pending_job()
            .unwrap()
            .unwrap()
            .0
            .pulsation_id,
        Some(b.id)
    );
    assert_eq!(f.count("runtime_slots"), 2);
}

#[test]
fn publication_waits_for_main_slot_but_not_for_optional_background() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", Some("integrated.md"), "final"))
        .unwrap();
    let (_root, project) = other_project(&f);
    let other = queue(&f, &project, PulsationStage::Specification);
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(job.pulsation_id, Some(other.id));
    let _slot = f
        .service
        .slot_guard(PersistenceService::job_slot_token(&job));
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    assert_eq!(f.state().status, PulsationStatus::Running);
    assert!(!f.workspace.path().join("integrated.md").exists());
    assert_eq!(state(&f).pipeline.base_epoch, 0);
    drop(_slot);
    f.service.reconcile_publications().unwrap();
    assert_eq!(f.state().status, PulsationStatus::Done);
}

#[test]
fn final_barrier_detects_edits_after_publication_ack() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", Some("final.md"), "generated"))
        .unwrap();
    let db = f.service.connection().unwrap();
    db.execute_batch("CREATE TRIGGER fail_done BEFORE UPDATE OF status ON pulsations WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'crash'); END;").unwrap();
    assert!(
        f.service
            .apply_hitl_decision(f.decision(HitlDecision::Approve))
            .is_err()
    );
    fs::write(f.workspace.path().join("final.md"), "human edit").unwrap();
    db.execute_batch("DROP TRIGGER fail_done").unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(f.state().status, PulsationStatus::Failed);
    assert_eq!(state(&f).pipeline.base_state, BaseState::NeedsReview);
    assert_eq!(state(&f).pipeline.base_epoch, 0);
}

#[test]
fn effects_require_explicit_review_with_fresh_hashes_and_keep_human_bytes() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "generated"))
        .unwrap();
    f.service.reconcile_publications().unwrap();
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    let b = queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
    let old = confirmation(&f, None);
    fs::write(f.workspace.path().join("code.rs"), "reviewed human change").unwrap();
    assert!(f.service.confirm_base_reconciliation(old).is_err());
    let stale = confirmation(&f, None);
    f.service
        .confirm_base_reconciliation(confirmation(&f, None))
        .unwrap();
    assert!(f.service.confirm_base_reconciliation(stale).is_err());
    assert_eq!(f.count("base_reconciliations"), 1);
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("code.rs")).unwrap(),
        "reviewed human change"
    );
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(job.pulsation_id, Some(b.id));
    let fence =
        PersistenceService::execution_fence(&f.service.connection().unwrap(), &job.id).unwrap();
    let handoff = f
        .service
        .load_stage_handoff(&fence, PulsationStage::Specification, 16000)
        .unwrap();
    assert_eq!(
        handoff.publication_bases[0].content_sha256,
        crate::orchestration::ContentSha256::of(b"reviewed human change")
    );
}

#[test]
fn legacy_owners_require_selection_and_each_held_successor_is_preserved() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "spec A")).unwrap();
    let b = queue(&f, &f.pulse.project_id, PulsationStage::Code);
    let db = f.service.connection().unwrap();
    db.execute("UPDATE project_pipeline_state SET active_pulsation_id=NULL,base_state='needs_review',blocking_reason='multiple_legacy_owners'",[]).unwrap();
    db.execute("UPDATE pulsations SET coordination_hold=1", [])
        .unwrap();
    assert!(f.service.claim_next_pending_job().unwrap().is_none());
    assert!(
        f.service
            .confirm_base_reconciliation(confirmation(&f, None))
            .is_err()
    );
    f.service
        .confirm_base_reconciliation(confirmation(&f, Some(f.pulse.id.clone())))
        .unwrap();
    assert!(
        f.service.claim_next_pending_job().unwrap().is_none(),
        "A still at HITL"
    );
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    assert_eq!(state(&f).pipeline.base_state, BaseState::NeedsReview);
    assert_eq!(state(&f).owner_candidates, vec![b.id.clone()]);
    f.service
        .confirm_base_reconciliation(confirmation(&f, Some(b.id.clone())))
        .unwrap();
    assert_eq!(
        f.service
            .claim_next_pending_job()
            .unwrap()
            .unwrap()
            .0
            .pulsation_id,
        Some(b.id)
    );
    assert_eq!(f.count("pulsations"), 2);
}

#[test]
fn recovery_retains_owner_and_clears_only_dead_execution_slots() {
    let f = Fixture::new(PulsationStage::Specification);
    queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    f.service.cancel_all_active_jobs();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        state(&f).pipeline.active_pulsation_id,
        Some(f.pulse.id.clone())
    );
    assert_eq!(f.count("runtime_slots"), 0);
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(job.pulsation_id, Some(f.pulse.id));
}

#[test]
fn cancellation_finishes_started_file_unit_but_prevents_the_next_one() {
    let f = Fixture::new(PulsationStage::Code);
    let mut output = f.output("advance", Some("one.rs"), "one");
    let mut second = output.artifacts[0].clone();
    second.relative_path = Some("two.rs".into());
    second.content = "two".into();
    output.artifacts.push(second);
    f.accept(&output).unwrap();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    *f.service.runtime_control.publication_gate.lock().unwrap() =
        Some((entered.clone(), release.clone()));
    std::thread::scope(|scope| {
        let publisher = scope.spawn(|| f.service.reconcile_publications());
        entered.wait(); // real filesystem replacement happened; SQLite ACK not yet committed
        let _intent = f.service.begin_user_cancellation(&f.pulse.id);
        let cancel = scope.spawn(|| {
            f.service.cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: f.pulse.id.clone(),
            })
        });
        release.wait();
        publisher.join().unwrap().unwrap();
        cancel.join().unwrap().unwrap();
    });
    let paths = ["one.rs", "two.rs"].map(|p| f.workspace.path().join(p).exists());
    assert_eq!(paths.iter().filter(|v| **v).count(), 1);
    assert_eq!(f.state().terminal_reason.as_deref(), Some("user_cancel"));
    assert_eq!(state(&f).pipeline.base_state, BaseState::NeedsReview);
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE state='applied'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE state='cancelled'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn idle_reconciliation_does_not_self_wake_and_release_wakes_an_already_registered_waiter() {
    use futures::FutureExt;
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "spec")).unwrap();
    let notified = f.service.runtime_control.wakeup.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    assert!(!f.service.reconcile_publications_until(None).unwrap());
    assert!(
        notified.as_mut().now_or_never().is_none(),
        "idle work must not spin its own loop"
    );
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    assert!(
        notified.as_mut().now_or_never().is_some(),
        "base release wakes immediately without waiting for the fallback timer"
    );
}

#[test]
fn old_integrator_approval_does_not_finalize_a_new_code_generation() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", None, "first review"))
        .unwrap();
    let request = f.decision(HitlDecision::RequestRevision);
    f.service.apply_hitl_decision(request).unwrap();
    let db = f.service.connection().unwrap();
    // Preserve an older approved checkpoint as can happen in imported history.
    db.execute("UPDATE checkpoints SET status='approved'", [])
        .unwrap();
    db.execute(
        "UPDATE pulsations SET stage='Integrator',status='running',state_version=state_version+1",
        [],
    )
    .unwrap();
    f.service.reconcile_publications().unwrap();
    assert_ne!(f.state().status, PulsationStatus::Done);
    assert_eq!(state(&f).pipeline.base_epoch, 0);
}

#[test]
fn supervisor_shutdown_joins_started_publication_and_recovers_remaining_unit() {
    let f = Fixture::new(PulsationStage::Code);
    let mut output = f.output("advance", Some("first.rs"), "first");
    let mut second = output.artifacts[0].clone();
    second.relative_path = Some("second.rs".into());
    output.artifacts.push(second);
    f.accept(&output).unwrap();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    *f.service.runtime_control.publication_gate.lock().unwrap() =
        Some((entered.clone(), release.clone()));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let stop = CancellationToken::new();
    let worker = f.service.clone();
    let token = stop.clone();
    let task = runtime.spawn(async move { worker.run_job_worker_until(token).await });
    entered.wait();
    stop.cancel();
    assert!(
        !task.is_finished(),
        "a running filesystem unit is still owned"
    );
    release.wait();
    runtime.block_on(task).unwrap();
    assert_eq!(f.count("runtime_slots"), 0);
    assert_eq!(
        state(&f).pipeline.active_pulsation_id,
        Some(f.pulse.id.clone())
    );
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE state='applied'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE state='pending'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE state='applied'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(f.count("agent_runs"), 1);
}

fn version_of(f: &Fixture, pulsation: &str) -> u64 {
    f.service
        .connection()
        .unwrap()
        .query_row(
            "SELECT state_version FROM pulsations WHERE id=?1",
            [pulsation],
            |r| r.get(0),
        )
        .unwrap()
}

fn status_of(f: &Fixture, pulsation: &str) -> PulsationStatus {
    PersistenceService::find_pulsation_by_id(&f.service.connection().unwrap(), pulsation)
        .unwrap()
        .unwrap()
        .status
}

#[test]
fn setup_failure_parks_on_a_decision_without_leaving_a_running_job() {
    let f = Fixture::new(PulsationStage::Specification);
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&f.job))
        .unwrap();
    let b = queue(&f, &f.pulse.project_id, PulsationStage::Specification);
    f.service.connection().unwrap().execute_batch("CREATE TRIGGER fail_start BEFORE INSERT ON agent_runs BEGIN SELECT RAISE(ABORT,'injected setup failure'); END;").unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(
        runtime
            .block_on(f.service.process_next_pending_job())
            .is_err()
    );
    assert_eq!(f.count("runtime_slots"), 0);
    // A setup failure is retryable, so the pulsation parks on a decision. The job is
    // `interrupted` (collected, resumable), never left `running`, and the project owner
    // stays held precisely because the work can still resume.
    assert_eq!(status_of(&f, &b.id), PulsationStatus::Checkpoint);
    assert!(state(&f).pipeline.active_pulsation_id.is_some());
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT status FROM jobs WHERE pulsation_id=?1",
            [&b.id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "interrupted"
    );
    let (checkpoint_id, allow_retry, allow_revision): (String, i64, i64) = db
        .query_row(
            "SELECT checkpoint_id,allow_retry,allow_revision FROM stage_failure_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    // Nothing to revise: the model never produced output.
    assert_eq!((allow_retry, allow_revision), (1, 0));
    drop(db);

    // Aborting is what finally releases the owner and flags the base.
    f.service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id,
            expected_state_version: version_of(&f, &b.id),
            decision: StageFailureDecision::Abort,
            feedback: None,
        })
        .unwrap();
    let aborted = PersistenceService::find_pulsation_by_id(&f.service.connection().unwrap(), &b.id)
        .unwrap()
        .unwrap();
    assert_eq!(aborted.status, PulsationStatus::Failed);
    assert_eq!(aborted.terminal_reason.as_deref(), Some("executor_setup"));
    assert!(state(&f).pipeline.active_pulsation_id.is_none());
    assert_eq!(
        f.service
            .connection()
            .unwrap()
            .query_row(
                "SELECT status FROM jobs WHERE pulsation_id=?1",
                [&b.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "failed"
    );
}

#[test]
fn manual_reconciliation_of_active_work_does_not_waive_final_hash_verification() {
    let f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", Some("reviewed.md"), "approved bytes"))
        .unwrap();
    let db = f.service.connection().unwrap();
    db.execute_batch("CREATE TRIGGER fail_done BEFORE UPDATE OF status ON pulsations WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'crash'); END;").unwrap();
    assert!(
        f.service
            .apply_hitl_decision(f.decision(HitlDecision::Approve))
            .is_err()
    );
    db.execute_batch("DROP TRIGGER fail_done; UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason='legacy_failure_unverified';").unwrap();
    f.service
        .confirm_base_reconciliation(confirmation(&f, Some(f.pulse.id.clone())))
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifacts WHERE reconciled_at IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    fs::write(
        f.workspace.path().join("reviewed.md"),
        "changed after manual review",
    )
    .unwrap();
    f.service.reconcile_publications().unwrap();
    assert_eq!(f.state().status, PulsationStatus::Failed);
    assert_eq!(state(&f).pipeline.base_state, BaseState::NeedsReview);
    assert_eq!(
        state(&f).pipeline.base_epoch,
        1,
        "only the manual adoption increment, never a successful integration"
    );
}
