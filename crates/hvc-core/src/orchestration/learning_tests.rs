use super::*;
use crate::persistence_service::serial::tests::Fixture;
use std::sync::Mutex;

struct Manual(Mutex<chrono::DateTime<Utc>>);
impl crate::llm::Clock for Manual {
    fn now(&self) -> chrono::DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn done() -> (Fixture, String, Arc<Manual>) {
    let mut f = Fixture::new(PulsationStage::Integrator);
    f.accept(&f.output("advance", None, "Accepted final result"))
        .unwrap();
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    assert_eq!(f.state().status, PulsationStatus::Done);
    let clock = Arc::new(Manual(Mutex::new(Utc::now())));
    f.service.clock = clock.clone();
    let id = f
        .service
        .connection()
        .unwrap()
        .query_row("SELECT job_id FROM learning_runs", [], |r| r.get(0))
        .unwrap();
    (f, id, clock)
}
fn state(f: &Fixture) -> (String, u32, u32) {
    f.service
        .connection()
        .unwrap()
        .query_row(
            "SELECT state,budget_calls,total_calls FROM learning_runs ORDER BY report_version DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}
fn release(f: &Fixture, claim: &BackgroundClaim) {
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&claim.job))
        .unwrap();
    f.service.unregister_job_cancellation(&claim.job.id);
}
fn report(input: &LearningInput) -> LearningReportV1 {
    LearningReportV1 {
        schema_version: 1,
        observations: vec![LearningFinding {
            text: "Review the accepted result".into(),
            evidence_ids: vec![input.evidence[0].id.clone()],
            recurring: false,
        }],
        recommendations: vec![],
        limitations: vec!["Local evidence only".into()],
    }
}
fn assert_done_unblocked(f: &Fixture) {
    assert_eq!(f.state().status, PulsationStatus::Done);
    let db = f.service.connection().unwrap();
    let base: (String, u64, Option<String>) = db
        .query_row(
            "SELECT base_state,base_epoch,active_pulsation_id FROM project_pipeline_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(base, ("ready".into(), 1, None));
}

#[test]
fn learning_intent_is_idempotent_and_disabled_completion_is_not_backfilled() {
    let (f, _, _) = done();
    let db = f.service.connection().unwrap();
    PersistenceService::enqueue_learning(&db, &f.pulse.id).unwrap();
    assert_eq!(f.count("learning_runs"), 1);
    assert_done_unblocked(&f);
    let f = Fixture::new(PulsationStage::Integrator);
    let mut flags = f.service.get_preparation_settings().unwrap();
    flags.learning_enabled = false;
    f.service
        .update_preparation_settings(flags.clone())
        .unwrap();
    f.accept(&f.output("advance", None, "final")).unwrap();
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    flags.learning_enabled = true;
    f.service.update_preparation_settings(flags).unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(f.count("learning_runs"), 0);
    assert_done_unblocked(&f);
}

#[test]
fn three_preemptions_defer_and_explicit_versioned_retry_preserves_total_usage() {
    let (f, id, clock) = done();
    for attempt in 1..=3 {
        let claim = f.service.claim_background_id(&id).unwrap().unwrap();
        f.service
            .begin_learning_attempt(&id, claim.generation)
            .unwrap();
        f.service
            .finish_learning_error(
                &claim,
                &LlmError::new(LlmErrorKind::Cancelled, "principal"),
                false,
            )
            .unwrap();
        release(&f, &claim);
        *clock.0.lock().unwrap() += chrono::Duration::seconds(5);
        assert_eq!(state(&f).1, attempt);
    }
    assert_eq!(state(&f), ("deferred_budget".into(), 3, 3));
    assert!(f.service.claim_background_id(&id).unwrap().is_none());
    f.service.recover_interrupted_jobs().unwrap();
    let mut flags = f.service.get_preparation_settings().unwrap();
    flags.learning_enabled = false;
    f.service
        .update_preparation_settings(flags.clone())
        .unwrap();
    flags.learning_enabled = true;
    f.service.update_preparation_settings(flags).unwrap();
    assert_eq!(state(&f), ("deferred_budget".into(), 3, 3));
    let (_, run) =
        PersistenceService::learning_snapshot(&f.service.connection().unwrap(), &f.pulse.id)
            .unwrap();
    let request = LearningActionRequest {
        job_id: id.clone(),
        expected_generation: run.unwrap().generation,
    };
    f.service.retry_learning(request.clone()).unwrap();
    assert!(f.service.retry_learning(request).is_err());
    assert_eq!(state(&f), ("pending".into(), 0, 3));
    assert_eq!(f.count("learning_runs"), 2);
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT budget_calls FROM learning_runs WHERE report_version=1",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        3
    );
    assert_done_unblocked(&f);
}

#[test]
fn one_transient_retry_and_admission_deferral_does_not_charge() {
    let (f, id, clock) = done();
    for (kind, expected) in [
        (LlmErrorKind::BackgroundDeferred, ("pending", 0)),
        (LlmErrorKind::Network, ("pending", 1)),
        (LlmErrorKind::Network, ("failed", 2)),
    ] {
        let claim = f.service.claim_background_id(&id).unwrap().unwrap();
        f.service
            .begin_learning_attempt(&id, claim.generation)
            .unwrap();
        f.service
            .finish_learning_error(&claim, &LlmError::new(kind, "fixture"), false)
            .unwrap();
        release(&f, &claim);
        *clock.0.lock().unwrap() += chrono::Duration::seconds(5);
        assert_eq!((state(&f).0, state(&f).1), (expected.0.into(), expected.1));
    }
    assert_done_unblocked(&f);
}

#[test]
fn frozen_input_is_bounded_and_recurrence_requires_two_distinct_pulses() {
    let (f, id, _) = done();
    let db = f.service.connection().unwrap();
    db.execute("UPDATE artifacts SET content=?1", ["á".repeat(30_000)])
        .unwrap();
    let claim = f.service.claim_background_id(&id).unwrap().unwrap();
    let input = f
        .service
        .begin_learning_attempt(&id, claim.generation)
        .unwrap();
    assert!(input.omitted > 0);
    assert!(serde_json::to_string(&input).unwrap().chars().count() <= 16_000);
    db.execute("UPDATE artifacts SET content='later edit'", [])
        .unwrap();
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        serde_json::to_string(&PersistenceService::learning_input(&db, &id).unwrap()).unwrap()
    );
    let mut output = report(&input);
    output.observations[0].recurring = true;
    assert!(output.validate(&input).is_err());
    let mut expanded = input.clone();
    expanded.evidence.push(LearningEvidence {
        id: "other".into(),
        pulsation_id: "previous".into(),
        kind: "historical_summary".into(),
        text: "same pattern".into(),
    });
    output.observations[0].evidence_ids.push("other".into());
    output.validate(&expanded).unwrap();
    output.observations[0].evidence_ids.push("invented".into());
    assert!(output.validate(&expanded).is_err());
    release(&f, &claim);
}

#[test]
fn proposed_outbox_replay_and_conflict_never_hold_next_pulse() {
    for conflict in [false, true] {
        let (f, id, _) = done();
        let claim = f.service.claim_background_id(&id).unwrap().unwrap();
        let input = f
            .service
            .begin_learning_attempt(&id, claim.generation)
            .unwrap();
        f.service
            .accept_learning_report(&id, claim.generation, &input, &report(&input))
            .unwrap();
        release(&f, &claim);
        let path = f
            .workspace
            .path()
            .join(format!("knowledge/obsidian/learning/{}-1.md", f.pulse.id));
        if conflict {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "human content").unwrap();
        }
        f.service.recover_interrupted_jobs().unwrap();
        assert!(
            f.service
                .reconcile_learning_publication(&CancellationToken::new())
                .unwrap()
        );
        assert!(
            !f.service
                .reconcile_learning_publication(&CancellationToken::new())
                .unwrap()
        );
        let content = fs::read_to_string(&path).unwrap();
        if conflict {
            assert_eq!(content, "human content");
            assert_eq!(state(&f).0, "conflict");
        } else {
            assert!(content.contains("status: proposed"));
            assert_eq!(state(&f).0, "published");
        }
        assert_done_unblocked(&f);
        f.service
            .create_pulsation(CreatePulsationRequest {
                project_id: f.pulse.project_id.clone(),
                feature_name: "next work".into(),
            })
            .unwrap();
        assert!(f.service.claim_next_pending_job().unwrap().is_some());
    }
}

#[test]
fn disable_recovery_and_dismiss_fence_late_reports_without_resetting_budget() {
    let (f, id, _) = done();
    let claim = f.service.claim_background_id(&id).unwrap().unwrap();
    let input = f
        .service
        .begin_learning_attempt(&id, claim.generation)
        .unwrap();
    let mut flags = f.service.get_preparation_settings().unwrap();
    flags.learning_enabled = false;
    f.service
        .update_preparation_settings(flags.clone())
        .unwrap();
    assert!(claim.cancellation.is_cancelled());
    f.service
        .accept_learning_report(&id, claim.generation, &input, &report(&input))
        .unwrap();
    assert_eq!(state(&f), ("disabled".into(), 1, 1));
    release(&f, &claim);
    flags.learning_enabled = true;
    f.service.update_preparation_settings(flags).unwrap();
    let claim = f.service.claim_background_id(&id).unwrap().unwrap();
    f.service
        .begin_learning_attempt(&id, claim.generation)
        .unwrap();
    release(&f, &claim);
    f.service.recover_interrupted_jobs().unwrap();
    f.service
        .accept_learning_report(&id, claim.generation, &input, &report(&input))
        .unwrap();
    assert_eq!(state(&f), ("pending".into(), 2, 2));
    let (_, run) =
        PersistenceService::learning_snapshot(&f.service.connection().unwrap(), &f.pulse.id)
            .unwrap();
    f.service
        .dismiss_learning(LearningActionRequest {
            job_id: id,
            expected_generation: run.unwrap().generation,
        })
        .unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(state(&f).0, "cancelled");
    assert_eq!(f.count("artifact_publications"), 0);
    assert_done_unblocked(&f);
}
