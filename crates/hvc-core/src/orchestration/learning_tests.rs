use super::*;
use crate::orchestration::ContractError;
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
fn report(input: &LearningInputV2) -> LearningReportV2 {
    LearningReportV2 {
        schema_version: 2,
        observations: vec![LearningFinding {
            text: "Review the accepted result".into(),
            evidence_ids: vec![input.evidence[0].id.clone()],
            recurring: false,
        }],
        recommendations: vec![],
        context_assessments: vec![],
        graph_selections: vec![],
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
fn learning_v2_rejects_invented_candidates_and_selection_without_operational_evidence() {
    let input = LearningInputV2 {
        pulsation_id: "pulse".into(),
        integration_epoch: 1,
        report_version: 2,
        refinement_mode: LearningRefinementMode::Propose,
        evidence: vec![LearningEvidence {
            id: "evidence:1".into(),
            pulsation_id: "pulse".into(),
            kind: "context_retrieval_item".into(),
            text: "delivered=true".into(),
        }],
        context_runs: vec![],
        graph_candidates: vec![LearningGraphCandidate {
            candidate_id: "candidate:1".into(),
            source_path: "root.md".into(),
            target_path: "test.md".into(),
            relation: "tested-by".into(),
            source_expected_sha256: "a".repeat(64),
            target_expected_sha256: "b".repeat(64),
            preview_diff: "+ tested-by".into(),
            evidence_ids: vec!["evidence:1".into()],
        }],
        omitted: 0,
    };
    let mut output = LearningReportV2 {
        schema_version: 2,
        observations: vec![],
        recommendations: vec![],
        context_assessments: vec![],
        graph_selections: vec![LearningGraphSelection {
            candidate_id: "invented".into(),
            evidence_ids: vec!["evidence:1".into()],
            rationale: "operational".into(),
        }],
        limitations: vec![],
    };
    assert!(output.validate(&input).is_err());
    output.graph_selections[0].candidate_id = "candidate:1".into();
    output.graph_selections[0].evidence_ids.clear();
    assert!(output.validate(&input).is_err());
    output.graph_selections.clear();
    output.validate(&input).unwrap();
    output
        .limitations
        .push("Relatorio gerado em portugues".into());
    assert!(matches!(
        output.validate(&input),
        Err(ContractError::Invalid("Learning report must be English"))
    ));
}

/// "Teste Haiku Only 1" (2026-09-24): a report over its list limits cost a
/// whole extra Learning call. The host trims it with a note; what it cannot
/// fix (unknown evidence) is still rejected, with a message that names it.
#[test]
fn learning_v2_trims_an_over_limit_report_instead_of_rejecting_it() {
    let input = LearningInputV2 {
        pulsation_id: "pulse".into(),
        integration_epoch: 1,
        report_version: 1,
        refinement_mode: LearningRefinementMode::Observe,
        evidence: vec![LearningEvidence {
            id: "evidence:1".into(),
            pulsation_id: "pulse".into(),
            kind: "stage_outcome".into(),
            text: "Integrator accepted".into(),
        }],
        context_runs: vec![],
        graph_candidates: vec![],
        omitted: 0,
    };
    let finding = |text: String| LearningFinding {
        text,
        evidence_ids: vec!["evidence:1".into(), "evidence:1".into()],
        recurring: false,
    };
    let mut report = LearningReportV2 {
        schema_version: 2,
        observations: (0..10)
            .map(|n| finding(format!("The stage reran step {n} after a review.")))
            .collect(),
        recommendations: vec![finding("Keep the review short. ".repeat(60))],
        context_assessments: vec![],
        graph_selections: vec![],
        limitations: (0..8)
            .map(|n| format!("Local evidence only {n}."))
            .collect(),
    };
    assert!(report.validate(&input).is_err());
    let trimmed = report.clamp_to_limits(&input);
    // 2 observations, 1 long text, 9 repeated ids, 1 limitation for the note.
    assert_eq!(trimmed, 13);
    report.validate(&input).unwrap();
    assert_eq!(report.observations.len(), 8);
    let text = &report.recommendations[0].text;
    assert_eq!(text.chars().count(), 1000);
    assert!(text.ends_with('…'));
    assert_eq!(report.limitations.len(), 8);
    assert_eq!(
        report.limitations.last().unwrap(),
        "Host trimmed 13 items to the report limits."
    );
    let markdown = report.markdown(&input).unwrap();
    assert!(markdown.contains("Host trimmed 13 items"));

    // A report within its limits is untouched.
    let mut clean = report.clone();
    assert_eq!(clean.clamp_to_limits(&input), 0);
    assert_eq!(clean.limitations, report.limitations);

    // Unknown evidence is a wrong answer, not a length: still rejected.
    report.observations[0].evidence_ids = vec!["evidence:9".into()];
    assert_eq!(report.clamp_to_limits(&input), 0);
    assert!(matches!(
        report.validate(&input),
        Err(ContractError::Invalid(
            "a Learning finding cites an evidence_id that was not supplied"
        ))
    ));
}

/// ToDoListWebView, 2026-09-22: an English report that quotes the developer's
/// pulsation titles (`teste 3`, `Teste 2`) was rejected as Portuguese. Titles
/// the host supplied as evidence are exempt; Portuguese prose still is not.
#[test]
fn quoting_host_supplied_pulsation_titles_is_not_non_english_prose() {
    let evidence = |id: &str, kind: &str, text: &str| LearningEvidence {
        id: id.into(),
        pulsation_id: "pulse".into(),
        kind: kind.into(),
        text: text.into(),
    };
    let input = LearningInputV2 {
        pulsation_id: "pulse".into(),
        integration_epoch: 1,
        report_version: 2,
        refinement_mode: LearningRefinementMode::Observe,
        evidence: vec![
            evidence("pulse:current", "completed_request", "Teste 4"),
            evidence("pulse:a", "historical_summary", "failed: teste 3"),
            evidence("pulse:b", "historical_summary", "failed: Teste 2"),
        ],
        context_runs: vec![],
        graph_candidates: vec![],
        omitted: 0,
    };
    let mut output = LearningReportV2 {
        schema_version: 2,
        observations: vec![],
        recommendations: vec![],
        context_assessments: vec![],
        graph_selections: vec![],
        limitations: vec![
            "Most failed runs carry terse titles such as 'teste 3', 'Teste 2' and 'Teste 4', so intent is hard to recover.".into(),
        ],
    };
    output.validate(&input).unwrap();
    output
        .limitations
        .push("Este teste falhou porque a tarefa ainda nao existe".into());
    assert!(matches!(
        output.validate(&input),
        Err(ContractError::Invalid("Learning report must be English"))
    ));
}

#[test]
fn switching_to_observe_during_learning_keeps_the_audit_but_discards_new_proposals() {
    let (f, learning_id, _) = done();
    let mut settings = f.service.get_preparation_settings().unwrap();
    settings.learning_refinement_mode = LearningRefinementMode::Propose;
    f.service
        .update_preparation_settings(settings.clone())
        .unwrap();
    let claim = f
        .service
        .claim_background_id(&learning_id)
        .unwrap()
        .unwrap();
    let mut input = f
        .service
        .begin_learning_attempt(&learning_id, claim.generation)
        .unwrap()
        .0;
    let evidence_id = input.evidence[0].id.clone();
    input.graph_candidates.push(LearningGraphCandidate {
        candidate_id: "candidate:observe-switch".into(),
        source_path: "knowledge/obsidian/features/root.md".into(),
        target_path: "knowledge/obsidian/tests/root.md".into(),
        relation: "tested-by".into(),
        source_expected_sha256: "a".repeat(64),
        target_expected_sha256: "b".repeat(64),
        preview_diff: "+ tested-by".into(),
        evidence_ids: vec![evidence_id.clone()],
    });
    let report = LearningReportV2 {
        schema_version: 2,
        observations: vec![],
        recommendations: vec![],
        context_assessments: vec![],
        graph_selections: vec![LearningGraphSelection {
            candidate_id: "candidate:observe-switch".into(),
            evidence_ids: vec![evidence_id],
            rationale: "Operational evidence from the fixture".into(),
        }],
        limitations: vec![],
    };
    report.validate(&input).unwrap();

    settings.learning_refinement_mode = LearningRefinementMode::Observe;
    f.service.update_preparation_settings(settings).unwrap();
    f.service
        .accept_learning_report(&learning_id, claim.generation, &input, &report)
        .unwrap();
    release(&f, &claim);

    assert_eq!(f.count("learning_graph_proposals"), 0);
    assert_eq!(f.count("learning_context_assessments"), 0);
    assert_eq!(
        f.service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE kind='learning_report'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "the audit report remains publishable"
    );
}

#[test]
fn failed_cancellation_schedules_learning_without_reopening_the_pulse() {
    let f = Fixture::new(PulsationStage::Code);
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    assert_eq!(f.state().status, PulsationStatus::Failed);
    assert_eq!(f.count("learning_runs"), 1);
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT status FROM jobs WHERE kind='learning_report'",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "pending"
    );
}

#[test]
fn graph_review_groups_source_patch_and_cas_preserves_external_edits() {
    for conflict in [false, true] {
        let (f, learning_id, _) = done();
        let mut settings = f.service.get_preparation_settings().unwrap();
        settings.learning_refinement_mode = LearningRefinementMode::Propose;
        f.service.update_preparation_settings(settings).unwrap();
        let feature_dir = f.workspace.path().join("knowledge/obsidian/features");
        let test_dir = f.workspace.path().join("knowledge/obsidian/tests");
        let contract_dir = f.workspace.path().join("knowledge/obsidian/contracts");
        fs::create_dir_all(&feature_dir).unwrap();
        fs::create_dir_all(&test_dir).unwrap();
        fs::create_dir_all(&contract_dir).unwrap();
        let source_path = "knowledge/obsidian/features/validated-contract.md";
        let test_path = "knowledge/obsidian/tests/validated-contract.md";
        let contract_path = "knowledge/obsidian/contracts/validated-contract.md";
        let source =
            "---\ntype: feature\nrisk: low\nstatus: active\n---\n# Implementar contrato validado\n";
        let test = "---\ntype: test\nrisk: low\nstatus: active\n---\n# Teste\n";
        let contract = "---\ntype: contract\nrisk: low\nstatus: active\n---\n# Contrato\n";
        fs::write(f.workspace.path().join(source_path), source).unwrap();
        fs::write(f.workspace.path().join(test_path), test).unwrap();
        fs::write(f.workspace.path().join(contract_path), contract).unwrap();
        let source_hash = ContentSha256::of(source.as_bytes());
        let test_hash = ContentSha256::of(test.as_bytes());
        let contract_hash = ContentSha256::of(contract.as_bytes());
        let db = f.service.connection().unwrap();
        db.execute(
            "UPDATE jobs SET status='done',finished_at=?2 WHERE id=?1",
            params![learning_id, PersistenceService::now_iso()],
        )
        .unwrap();
        db.execute(
            "UPDATE learning_runs SET state='published' WHERE job_id=?1",
            [&learning_id],
        )
        .unwrap();
        let generation: u64 = db
            .query_row(
                "SELECT generation FROM jobs WHERE id=?1",
                [&learning_id],
                |row| row.get(0),
            )
            .unwrap();
        let mut proposal_ids = Vec::new();
        for (candidate, target, relation, target_hash) in [
            ("candidate-test", test_path, "tested-by", test_hash.as_str()),
            (
                "candidate-contract",
                contract_path,
                "contracts",
                contract_hash.as_str(),
            ),
        ] {
            let proposal_id = Uuid::new_v4().to_string();
            db.execute(
                "INSERT INTO learning_graph_proposals(id,learning_job_id,project_id,pulsation_id,candidate_id,source_path,target_path,relation,source_expected_sha256,target_expected_sha256,preview_diff,rationale,evidence_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'preview','evidence','[\"evidence\"]',?11,?11)",
                params![proposal_id,learning_id,f.pulse.project_id,f.pulse.id,candidate,source_path,target,relation,source_hash.as_str(),target_hash,PersistenceService::now_iso()],
            ).unwrap();
            proposal_ids.push(proposal_id);
        }
        let review = ReviewLearningGraphRequest {
            job_id: learning_id.clone(),
            expected_generation: generation,
            decisions: proposal_ids
                .iter()
                .map(|id| ReviewLearningGraphDecision {
                    proposal_id: id.clone(),
                    decision: LearningGraphDecision::Approve,
                })
                .collect(),
        };
        f.service.review_learning_graph(review.clone()).unwrap();
        assert!(
            f.service.review_learning_graph(review).is_err(),
            "a repeated or stale review click must not create another artifact"
        );
        assert_eq!(
            fs::read_to_string(f.workspace.path().join(source_path)).unwrap(),
            source
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE kind='knowledge_graph_patch'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1,
            "duas alterações na mesma nota devem compor um artifact"
        );
        if conflict {
            fs::write(f.workspace.path().join(source_path), "human edit").unwrap();
        }
        f.service.recover_interrupted_jobs().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM artifact_publications WHERE state='pending'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1,
            "recovery must preserve an approved graph publication"
        );
        if !conflict {
            db.execute(
                "UPDATE artifact_publications SET state='applying' WHERE state='pending'",
                [],
            )
            .unwrap();
            db.execute(
                "UPDATE learning_graph_proposals SET publication_state='applying' WHERE publication_state='pending'",
                [],
            )
            .unwrap();
            let mut settings = f.service.get_preparation_settings().unwrap();
            settings.learning_enabled = false;
            settings.learning_refinement_mode = LearningRefinementMode::Observe;
            f.service.update_preparation_settings(settings).unwrap();
        }
        assert!(
            f.service
                .reconcile_learning_publication(&CancellationToken::new())
                .unwrap()
        );
        let current = fs::read_to_string(f.workspace.path().join(source_path)).unwrap();
        if conflict {
            assert_eq!(current, "human edit");
            assert_eq!(
                db.query_row(
                    "SELECT MIN(publication_state) FROM learning_graph_proposals",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
                "conflict"
            );
        } else {
            assert!(current.contains("tested-by:"));
            assert!(current.contains("contracts:"));
            let mut cache = crate::workspace_service::VaultIndexCache::default();
            let candidates = crate::workspace_service::get_context_candidates(
                f.workspace.path(),
                &mut cache,
                crate::workspace_service::GetContextCandidatesRequest {
                    feature_name: "Implementar contrato validado".into(),
                    stage_name: None,
                    agent_scope: None,
                    max_candidates: 8,
                    force_rebuild: true,
                },
            )
            .unwrap();
            assert!(
                candidates
                    .required_context_paths
                    .contains(&test_path.to_string())
            );
            assert!(
                candidates
                    .required_context_paths
                    .contains(&contract_path.to_string())
            );
        }
        assert_done_unblocked(&f);
    }
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
fn human_blocked_foreground_does_not_starve_learning_but_claimable_work_does() {
    let (f, learning_id, clock) = done();
    let mut settings = f.service.get_preparation_settings().unwrap();
    settings.preparation_enabled = true;
    settings.vagueness_threshold = 40.try_into().unwrap();
    f.service.update_preparation_settings(settings).unwrap();
    let human_blocked_root = tempfile::tempdir().unwrap();
    let human_blocked_project = f
        .service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: human_blocked_root.path().to_string_lossy().into_owned(),
            preferred_name: None,
        })
        .unwrap()
        .project;
    let human_blocked = f
        .service
        .create_pulsation(CreatePulsationRequest {
            project_id: human_blocked_project.id,
            feature_name: "melhore o sistema".into(),
        })
        .unwrap()
        .pulsation;
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT review_state FROM pulsation_preparation WHERE pulsation_id=?1",
            [&human_blocked.id],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "pending"
    );
    assert_eq!(
        PersistenceService::learning_candidate(&db, &clock.now().to_rfc3339())
            .unwrap()
            .map(|candidate| candidate.0),
        Some(learning_id)
    );

    let claimable_root = tempfile::tempdir().unwrap();
    let claimable_project = f
        .service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: claimable_root.path().to_string_lossy().into_owned(),
            preferred_name: None,
        })
        .unwrap()
        .project;
    f.service
        .create_pulsation(CreatePulsationRequest {
            project_id: claimable_project.id,
            feature_name: "Validar JWT em auth.rs e retornar false para token inválido".into(),
        })
        .unwrap();
    assert!(
        PersistenceService::learning_candidate(&db, &clock.now().to_rfc3339())
            .unwrap()
            .is_none()
    );
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
            "SELECT budget_calls FROM learning_runs WHERE report_version=2",
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
        .unwrap()
        .0;
    assert!(input.omitted > 0);
    assert!(serde_json::to_string(&input).unwrap().chars().count() <= 16_000);
    db.execute("UPDATE artifacts SET content='later edit'", [])
        .unwrap();
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        serde_json::to_string(&f.service.learning_input(&db, &id).unwrap()).unwrap()
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
            .unwrap()
            .0;
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
            assert!(content.contains("language: en"));
            assert!(content.contains("# Consultative learning report"));
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
        .unwrap()
        .0;
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

/// Field incident, 2026-09-10: the most expensive call of the whole pulsation
/// produced a report that was merely written in the wrong language, and the run
/// died on its first of three granted calls. Five of eleven historical Learning
/// failures were this exact rejection, and two more were unparseable JSON -
/// seven failures a second sample could have fixed, all routed through
/// `Validation`, which is never retried.
#[test]
fn a_report_rejected_for_its_language_is_retryable_and_the_retry_is_told_why() {
    let (f, id, _) = done();
    let claim = f.service.claim_background_id(&id).unwrap().unwrap();
    let (input, first_rejection) = f
        .service
        .begin_learning_attempt(&id, claim.generation)
        .unwrap();
    // Nothing has been rejected yet on the first call of a run.
    assert_eq!(first_rejection, None);
    assert_eq!(state(&f), ("running".into(), 1, 1));

    let mut portuguese = report(&input);
    portuguese.observations[0].text = "Revisar o resultado aprovado da etapa".into();
    let error = f
        .service
        .accept_learning_report(&id, claim.generation, &input, &portuguese)
        .expect_err("the English contract rejects the report");
    assert!(
        matches!(error, PersistenceServiceError::ModelOutputInvalid(_)),
        "language rejections must be retryable, got {error:?}"
    );
    assert!(error.to_string().contains("must be English"));

    // What the background lane does with that error: park the run back on
    // `pending` with the diagnostic, instead of ending it.
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE learning_runs SET state='pending',diagnostic=?1 WHERE job_id=?2",
            params![error.to_string(), id],
        )
        .unwrap();
    release(&f, &claim);
    f.service.recover_interrupted_jobs().unwrap();

    let claim = f.service.claim_background_id(&id).unwrap().unwrap();
    let (_, second_rejection) = f
        .service
        .begin_learning_attempt(&id, claim.generation)
        .unwrap();
    assert!(
        second_rejection
            .as_deref()
            .is_some_and(|rejection| rejection.contains("must be English")),
        "the corrective retry must carry the reason forward, got {second_rejection:?}"
    );
    // The second of the three granted calls is now actually spent.
    assert_eq!(state(&f), ("running".into(), 2, 2));

    // English passes the same contract.
    f.service
        .accept_learning_report(&id, claim.generation, &input, &report(&input))
        .unwrap();
    assert_eq!(state(&f).0, "publishing");
}
