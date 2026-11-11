use super::*;
use crate::orchestration::ClarificationAnswerV1;
use crate::orchestration::ContextPackageV1;
use crate::orchestration::PreSpecSkipReason;
use crate::orchestration::StageHandoffV1;
use crate::orchestration::SubmitSpecificationAnswersRequest;
use crate::persistence_service::ApplyHitlDecisionRequest;
use crate::persistence_service::CancelPulsationJobsRequest;
use crate::persistence_service::CreatePulsationRequest;
use crate::persistence_service::HitlDecision;
use crate::persistence_service::serial::tests::Fixture;
use serde_json::json;
use std::fs;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn enabled(service: &PersistenceService, threshold: u8) {
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: true,
            vagueness_threshold: threshold.try_into().unwrap(),
            ..Default::default()
        })
        .unwrap();
}
fn create(f: &Fixture, text: &str) -> Pulsation {
    f.service
        .create_pulsation(CreatePulsationRequest {
            project_id: f.pulse.project_id.clone(),
            feature_name: text.into(),
        })
        .unwrap()
        .pulsation
}
fn detail(f: &Fixture, pulse: &str) -> PulsationCoordinationSnapshot {
    PersistenceService::coordination_details(&f.service.connection().unwrap(), &f.pulse.project_id)
        .unwrap()
        .into_iter()
        .find(|d| d.pulsation_id == pulse)
        .unwrap()
}
async fn waiting_successor() -> (Fixture, Pulsation) {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output(
        "checkpoint_ready",
        None,
        "The specification is approved only after HITL review",
    ))
    .unwrap();
    fs::write(
        f.workspace.path().join("auth.rs"),
        "use crate::tokens;\npub fn validate_token() -> bool { false }\n",
    )
    .unwrap();
    enabled(&f.service, 40);
    let pulse = create(
        &f,
        "crie validação em auth.rs quando token inválido deve retornar 401",
    );
    assert!(
        f.service
            .process_anticipation(&CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(detail(&f, &pulse.id).stage, PulsationStage::Specification);
    (f, pulse)
}

#[test]
fn description_review_is_local_versioned_unicode_and_not_silently_confirmed() {
    let f = Fixture::new(PulsationStage::Specification);
    enabled(&f.service, 40);
    let pulse = create(&f, "  melhore o sistema  ");
    let d = detail(&f, &pulse.id);
    assert_eq!(
        d.preparation.description_review,
        DescriptionReviewState::Pending
    );
    assert_eq!(pulse.feature_name, "  melhore o sistema  ");
    assert_eq!(d.preparation.assessment.unwrap().score.get(), 65);
    assert!(f.service.claim_anticipation().unwrap().is_none());
    f.service
        .update_preparation_settings(PreparationSettings::default())
        .unwrap();
    assert_eq!(
        detail(&f, &pulse.id).preparation.description_review,
        DescriptionReviewState::Pending
    );
    f.service
        .confirm_description_review(ConfirmDescriptionReviewRequest {
            pulsation_id: pulse.id.clone(),
            expected_input_revision: 1,
        })
        .unwrap();
    assert!(
        f.service
            .confirm_description_review(ConfirmDescriptionReviewRequest {
                pulsation_id: pulse.id.clone(),
                expected_input_revision: 1
            })
            .is_err()
    );
    let revised = f
        .service
        .revise_pulsation_description(RevisePulsationDescriptionRequest {
            pulsation_id: pulse.id.clone(),
            expected_input_revision: 1,
            description: "Crie ação Unicode em auth.rs".into(),
        })
        .unwrap();
    assert_eq!(revised.feature_name, "Crie ação Unicode em auth.rs");
    assert_eq!(detail(&f, &pulse.id).input_revision, 2);
    assert!(
        f.service
            .confirm_description_review(ConfirmDescriptionReviewRequest {
                pulsation_id: pulse.id.clone(),
                expected_input_revision: 1
            })
            .is_err()
    );
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM pulsation_description_revisions WHERE pulsation_id=?1",
            [pulse.id],
            |r| r.get::<_, usize>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        1,
        "TaskRefine never calls a provider"
    );
    assert!(
        f.service
            .revise_pulsation_description(RevisePulsationDescriptionRequest {
                pulsation_id: f.pulse.id.clone(),
                expected_input_revision: 1,
                description: "Outro pedido".into()
            })
            .is_err()
    );
}

#[test]
fn threshold_edges_apply_only_to_next_assessment() {
    let f = Fixture::new(PulsationStage::Specification);
    enabled(&f.service, 0);
    let pulse = create(&f, "crie validação auth token");
    enabled(&f.service, 100);
    assert_eq!(
        detail(&f, &create(&f, "melhorias etc").id)
            .preparation
            .description_review,
        DescriptionReviewState::Pending
    );
    assert_eq!(
        detail(&f, &pulse.id)
            .preparation
            .assessment
            .unwrap()
            .threshold
            .get(),
        0
    );
    assert_eq!(
        detail(&f, &create(&f, "melhore o sistema").id)
            .preparation
            .description_review,
        DescriptionReviewState::NotRequired
    );
    assert!(
        f.service
            .create_pulsation(CreatePulsationRequest {
                project_id: f.pulse.project_id.clone(),
                feature_name: "á".repeat(16001)
            })
            .is_err()
    );
    assert!(
        f.service
            .create_pulsation(CreatePulsationRequest {
                project_id: f.pulse.project_id.clone(),
                feature_name: "á".repeat(16000)
            })
            .is_ok()
    );
}

#[tokio::test]
async fn superseded_preparation_cannot_change_the_new_description_and_recovery_does_not_rescan() {
    let (f, pulse) = waiting_successor().await;
    let (job, token) = f.service.claim_anticipation().unwrap().unwrap();
    f.service
        .revise_pulsation_description(RevisePulsationDescriptionRequest {
            pulsation_id: pulse.id.clone(),
            expected_input_revision: 1,
            description: "crie segunda validação em auth.rs".into(),
        })
        .unwrap();
    assert!(token.is_cancelled());
    f.service.execute_preparation(&job, &token).unwrap();
    assert_eq!(detail(&f, &pulse.id).input_revision, 2);
    assert_eq!(
        detail(&f, &pulse.id).preparation.prespec_state,
        PreSpecState::NotScheduled
    );
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&job))
        .unwrap();
    f.service
        .process_anticipation(&CancellationToken::new())
        .await
        .unwrap();
    let (job, _) = f.service.claim_anticipation().unwrap().unwrap();
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        detail(&f, &pulse.id).preparation.prespec_state,
        PreSpecState::Cancelled
    );
    assert!(f.service.claim_anticipation().unwrap().is_none());
    assert_eq!(
        f.service
            .connection()
            .unwrap()
            .query_row("SELECT attempts FROM jobs WHERE id=?1", [job.id], |r| r
                .get::<_, usize>(
                0
            ))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn kill_switch_cancels_unconsumed_context_but_keeps_main_progress_possible() {
    let (f, pulse) = waiting_successor().await;
    let (job, token) = f.service.claim_anticipation().unwrap().unwrap();
    f.service
        .update_preparation_settings(PreparationSettings::default())
        .unwrap();
    assert!(token.is_cancelled());
    f.service.execute_preparation(&job, &token).unwrap();
    assert_eq!(
        detail(&f, &pulse.id).preparation.skip_reason,
        Some(PreSpecSkipReason::Disabled)
    );
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    let (main, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(main.pulsation_id, Some(pulse.id));
}

#[tokio::test]
async fn only_immediate_successor_intake_and_one_local_preparation_are_admitted() {
    let (f, pulse) = waiting_successor().await;
    let third = create(&f, "crie outra função em auth.rs");
    let before = f
        .service
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM agent_runs", [], |r| {
            r.get::<_, usize>(0)
        })
        .unwrap();
    assert!(
        f.service
            .process_anticipation(&CancellationToken::new())
            .await
            .unwrap()
    );
    let d = detail(&f, &pulse.id);
    assert_eq!(d.preparation.prespec_state, PreSpecState::Ready);
    let content = f
        .service
        .read_artifact_content(
            &pulse.id,
            d.preparation.context_artifact_id.as_ref().unwrap(),
        )
        .unwrap();
    let package: ContextPackageV1 = serde_json::from_str(&content).unwrap();
    assert_eq!(package.facts.sources[0].relative_path.as_str(), "auth.rs");
    assert_eq!(detail(&f, &third.id).stage, PulsationStage::Intake);
    fs::write(f.workspace.path().join("auth.rs"), "new source body").unwrap();
    assert!(
        !f.service
            .process_anticipation(&CancellationToken::new())
            .await
            .unwrap()
    );
    let db = f.service.connection().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM artifact_publications WHERE pulsation_id=?1",
            [pulse.id],
            |r| r.get::<_, usize>(0)
        )
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn completed_package_is_consumed_or_invalidated_by_body_and_candidate_changes() {
    for change in ["none", "body", "same_metadata", "add", "delete", "rename"] {
        let (f, pulse) = waiting_successor().await;
        f.service
            .process_anticipation(&CancellationToken::new())
            .await
            .unwrap();
        match change {
            "body" => fs::write(
                f.workspace.path().join("auth.rs"),
                "pub fn validate_token() -> bool { true }\n",
            )
            .unwrap(),
            "same_metadata" => {
                let path = f.workspace.path().join("auth.rs");
                let before = fs::metadata(&path).unwrap();
                let content = fs::read_to_string(&path).unwrap();
                fs::write(&path, content.replace("false", "true ")).unwrap();
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
                    .unwrap();
                let after = fs::metadata(&path).unwrap();
                assert_eq!(after.len(), before.len());
                assert_eq!(after.modified().unwrap(), before.modified().unwrap());
            }
            "add" => fs::write(
                f.workspace.path().join("auth_test.rs"),
                "#[test] fn auth_token() {}\n",
            )
            .unwrap(),
            "delete" => fs::remove_file(f.workspace.path().join("auth.rs")).unwrap(),
            "rename" => fs::rename(
                f.workspace.path().join("auth.rs"),
                f.workspace.path().join("auth_renamed.rs"),
            )
            .unwrap(),
            _ => {}
        }
        f.service
            .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: f.pulse.id.clone(),
            })
            .unwrap();
        f.service.process_next_pending_job().await.unwrap();
        let d = detail(&f, &pulse.id);
        assert_eq!(
            d.context_disposition.as_deref(),
            Some(if change == "none" {
                "reused"
            } else {
                "stale_on_demand"
            }),
            "{change}"
        );
        let db = f.service.connection().unwrap();
        let metrics: Vec<String> = db.prepare("SELECT message FROM pulse_events WHERE pulsation_id=?1 AND event_type='context_collected'").unwrap().query_map([&pulse.id], |r| r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
        assert_eq!(
            metrics.len(),
            2,
            "one accepted preparation and one fenced revalidation"
        );
        for metric in metrics {
            let value: serde_json::Value = serde_json::from_str(&metric).unwrap();
            assert!(value["elapsed_us"].is_u64());
            assert!(value["read_bytes"].is_u64());
            assert!(value["enumerated_files"].is_u64());
            assert!(!metric.contains("auth.rs") && !metric.contains("validate_token"));
            assert!(value["snippets"].is_null() && value["diagnostics"].is_null());
        }
        let json:String=db.query_row("SELECT handoff_json FROM stage_acceptances a JOIN jobs j ON j.id=a.job_id WHERE j.pulsation_id=?1 AND j.kind='run_stage::SPECIFICATION'",[&pulse.id],|r|r.get(0)).unwrap();
        let handoff: StageHandoffV1 = serde_json::from_str(&json).unwrap();
        assert!(handoff.context.is_some());
        if matches!(change, "body" | "same_metadata") {
            assert!(json.contains("true "));
            assert!(!json.contains("false }"));
        }
        if change == "delete" {
            assert!(handoff.context.unwrap().facts.sources.is_empty());
        }
    }
}

#[tokio::test]
async fn main_claim_does_not_wait_for_inflight_local_read_or_accept_its_late_result() {
    let (f, pulse) = waiting_successor().await;
    let (job, cancellation) = f.service.claim_anticipation().unwrap().unwrap();
    let entered = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    *f.service.runtime_control.preparation_gate.lock().unwrap() =
        Some((entered.clone(), release.clone()));
    let service = f.service.clone();
    let job_copy = job.clone();
    let token = cancellation.clone();
    let worker = std::thread::spawn(move || service.execute_preparation(&job_copy, &token));
    entered.wait();
    f.service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: f.pulse.id.clone(),
        })
        .unwrap();
    let (main, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    assert_eq!(main.pulsation_id.as_deref(), Some(pulse.id.as_str()));
    assert!(cancellation.is_cancelled());
    assert!(f.service.claim_anticipation().unwrap().is_none());
    release.wait();
    worker.join().unwrap().unwrap();
    assert!(
        detail(&f, &pulse.id)
            .preparation
            .context_artifact_id
            .is_none()
    );
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&job))
        .unwrap();
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&main))
        .unwrap();
}

#[tokio::test]
async fn ambiguous_waiver_skips_preparation_but_allows_intake() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("checkpoint_ready", None, "spec"))
        .unwrap();
    enabled(&f.service, 40);
    let pulse = create(&f, "melhore auth token");
    assert!(
        !f.service
            .process_anticipation(&CancellationToken::new())
            .await
            .unwrap()
    );
    f.service
        .confirm_description_review(ConfirmDescriptionReviewRequest {
            pulsation_id: pulse.id.clone(),
            expected_input_revision: 1,
        })
        .unwrap();
    f.service
        .process_anticipation(&CancellationToken::new())
        .await
        .unwrap();
    assert!(
        !f.service
            .process_anticipation(&CancellationToken::new())
            .await
            .unwrap()
    );
    assert_eq!(
        detail(&f, &pulse.id).preparation.skip_reason,
        Some(PreSpecSkipReason::AmbiguousRequest)
    );
}

#[test]
fn clarification_checkpoint_survives_restart_and_answers_cannot_approve_artifacts() {
    let f = Fixture::new(PulsationStage::Specification);
    let mut output = f.output("needs_clarification", None, "unused");
    output.artifacts.clear();
    output.structured_output = json!({"outputLanguage":"en","summary":"A human decision is required","statusUpdate":"needs_clarification","contextUsage":[],"notes":[],"artifacts":[],"questions":[{"id":"auth","question":"Which token format should be used?","rationale":"Defines the authentication contract"}]});
    f.accept(&output).unwrap();
    let pending = detail(&f, &f.pulse.id).questions.unwrap();
    assert!(
        f.service
            .apply_hitl_decision(ApplyHitlDecisionRequest {
                pulsation_id: f.pulse.id.clone(),
                checkpoint_id: pending.checkpoint_id.clone(),
                expected_state_version: pending.fence.state_version,
                decision: HitlDecision::Approve,
                feedback: None
            })
            .is_err()
    );
    f.service.recover_interrupted_jobs().unwrap();
    assert_eq!(detail(&f, &f.pulse.id).questions.unwrap(), pending);
    let request = SubmitSpecificationAnswersRequest {
        pulsation_id: f.pulse.id.clone(),
        question_set_id: pending.id.clone(),
        checkpoint_id: pending.checkpoint_id.clone(),
        expected_input_revision: 1,
        expected_state_version: pending.fence.state_version,
        answers: vec![ClarificationAnswerV1 {
            question_id: "auth".into(),
            text_answer: "JWT assinado".into(),
            planned_paths: Vec::new(),
            modified_paths: Vec::new(),
        }],
    };
    let mut stale = request.clone();
    stale.expected_state_version -= 1;
    assert!(f.service.submit_specification_answers(stale).is_err());
    let mut incomplete = request.clone();
    incomplete.answers.clear();
    assert!(f.service.submit_specification_answers(incomplete).is_err());
    f.service
        .submit_specification_answers(request.clone())
        .unwrap();
    assert!(f.service.submit_specification_answers(request).is_err());
    let receipt = f
        .service
        .checkpoint_receipt(&pending.checkpoint_id)
        .unwrap()
        .expect("the answers have a receipt");
    assert_eq!(receipt.purpose, "clarification");
    assert_eq!(receipt.decision, "answered");
    assert!(receipt.feedback_sha256.is_some());
    assert!(
        !serde_json::to_string(&receipt)
            .unwrap()
            .contains("JWT assinado"),
        "answers are kept as a digest only"
    );
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    let db = f.service.connection().unwrap();
    let fence = PersistenceService::execution_fence(&db, &job.id).unwrap();
    assert!(fence.generation > pending.fence.generation);
    let handoff = f
        .service
        .load_stage_handoff(&fence, PulsationStage::Specification, 16000)
        .unwrap();
    assert_eq!(handoff.clarifications[0].questions[0].id, "auth");
    assert_eq!(handoff.answers[0].text_answer, "JWT assinado");
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE conversation_id=?1",
            [pending.conversation_id],
            |r| r.get::<_, usize>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        0
    );
}
