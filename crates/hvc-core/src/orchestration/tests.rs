use std::collections::HashSet;

use serde_json::json;

use crate::persistence_service::{JobStatus, PulsationStage};

use super::*;

fn fence() -> ExecutionFence {
    ExecutionFence {
        project_id: "project-a".into(),
        pulsation_id: Some("pulse-a".into()),
        input_revision: Some(1),
        job_id: "job-a".into(),
        generation: 1,
        state_version: 3,
    }
}

fn completed() -> StageOutcome {
    StageOutcome::Completed {
        summary: "Concluído".into(),
        notes: vec![],
        artifacts: vec![],
    }
}

fn question() -> SpecificationQuestion {
    SpecificationQuestion {
        id: "question-a".into(),
        question: "Qual comportamento é esperado?".into(),
        rationale: "O pedido não define o resultado.".into(),
    }
}

fn questions() -> SpecificationQuestionSet {
    SpecificationQuestionSet {
        id: "set-a".into(),
        checkpoint_id: "checkpoint-a".into(),
        fence: fence(),
        conversation_id: "conversation-a".into(),
        questions: vec![question()],
    }
}

fn answers() -> SubmitSpecificationAnswersRequest {
    SubmitSpecificationAnswersRequest {
        pulsation_id: "pulse-a".into(),
        question_set_id: "set-a".into(),
        checkpoint_id: "checkpoint-a".into(),
        expected_input_revision: 1,
        expected_state_version: 3,
        answers: vec![SpecificationAnswer {
            question_id: "question-a".into(),
            answer: "Exibir o total.".into(),
        }],
    }
}

fn source(project: &str, content: &str) -> SourceSnapshot {
    SourceSnapshot::from_text(
        project,
        RelativeSourcePath::new("src/auth.rs").unwrap(),
        content.into(),
    )
    .unwrap()
}

fn extractor() -> ExtractorRevision {
    ExtractorRevision {
        name: "lexical".into(),
        version: "1".into(),
        policy_version: "1".into(),
        resolution_context: None,
    }
}

fn facts(snapshot: &SourceSnapshot) -> CodeContextFacts {
    CodeContextFacts {
        schema_version: SchemaV1,
        project_id: snapshot.revision().project_id.clone(),
        extractor: extractor(),
        scope: "src/auth.rs".into(),
        sources: vec![snapshot.revision().clone()],
        snippets: vec![snapshot.snippet(1, 1).unwrap()],
        references: vec![],
        coverage: ContextCoverage {
            state: CoverageState::Partial,
            documentation: DocumentationAvailability::Absent,
            enumerated_files: 1,
            read_bytes: snapshot.content().len() as u64,
            truncated: false,
            diagnostics: vec!["Coletor lexical; grafo completo não disponível.".into()],
        },
    }
}

fn handoff() -> StageHandoffV1 {
    let content = "# Especificação aprovada\n".to_owned();
    let mut producer = fence();
    producer.job_id = "job-spec".into();
    producer.state_version = 2;
    StageHandoffV1 {
        schema_version: SchemaV1,
        recipient: fence(),
        stage: PulsationStage::Contract,
        assessment: None,
        context: None,
        answers: vec![],
        revision_feedback: None,
        artifacts: vec![ArtifactHandoff {
            artifact_id: "artifact-spec".into(),
            producer,
            producer_stage: PulsationStage::Specification,
            output_position: 0,
            name: "Especificação".into(),
            kind: "spec".into(),
            content_sha256: ContentSha256::of(content.as_bytes()),
            content,
            acceptance: ArtifactAcceptance::Accepted,
            publication: PublicationState::NotRequired,
            source_revisions: vec![],
        }],
    }
}

#[test]
fn preserves_exactly_the_six_existing_stage_wire_values() {
    let stages = [
        "Intake",
        "SPECIFICATION",
        "Contract",
        "Code",
        "Test",
        "Integrator",
    ];
    for stage in stages {
        let parsed: PulsationStage = serde_json::from_value(json!(stage)).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), json!(stage));
    }
    for name in ["TaskRefine", "PreSpec", "Learning"] {
        assert!(serde_json::from_value::<PulsationStage>(json!(name)).is_err());
    }
    assert_eq!(LEGACY_PIPELINE_VERSION, 1);
    assert_eq!(CURRENT_PIPELINE_VERSION, 2);
}

#[test]
fn settings_default_to_disabled_preparation_and_validate_score_boundaries() {
    let settings: PreparationSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(settings, PreparationSettings::default());
    assert!(!settings.preparation_enabled);
    assert!(settings.learning_enabled);
    assert_eq!(settings.vagueness_threshold.get(), 40);
    for value in [0, 40, 100] {
        assert_eq!(
            serde_json::from_value::<VaguenessScore>(json!(value))
                .unwrap()
                .get(),
            value
        );
    }
    for value in [-1, 101, 256] {
        assert!(serde_json::from_value::<VaguenessScore>(json!(value)).is_err());
    }
}

#[test]
fn description_limit_counts_unicode_characters_not_utf8_bytes() {
    let mut request = RevisePulsationDescriptionRequest {
        pulsation_id: "pulse-a".into(),
        expected_input_revision: 1,
        description: "á".repeat(16_000),
    };
    request.validate().unwrap();
    request.description.push('á');
    assert!(request.validate().is_err());
    request.description = " \n\t".into();
    assert!(request.validate().is_err());
}

#[test]
fn rejects_every_changed_fencing_dimension() {
    let submitted = fence();
    validate_result_acceptance(&submitted, &submitted, JobStatus::Running, None).unwrap();
    for dimension in 0..7 {
        let mut current = submitted.clone();
        match dimension {
            0 => current.project_id = "other-project".into(),
            1 => current.pulsation_id = Some("other-pulse".into()),
            2 => current.input_revision = Some(2),
            3 => current.job_id = "other-job".into(),
            4 => current.generation += 1,
            5 => current.state_version += 1,
            _ => {
                current.pulsation_id = None;
                current.input_revision = None;
            }
        }
        assert_eq!(
            validate_result_acceptance(&submitted, &current, JobStatus::Running, None),
            Err(ContractError::StaleExecution)
        );
    }
}

#[test]
fn closed_or_cancelled_job_cannot_accept_a_matching_result() {
    let current = fence();
    for status in [
        JobStatus::Pending,
        JobStatus::Done,
        JobStatus::Failed,
        JobStatus::Interrupted,
    ] {
        assert_eq!(
            validate_result_acceptance(&current, &current, status, None),
            Err(ContractError::ExecutionClosed)
        );
    }
    for reason in [
        CancellationReason::UserCancel,
        CancellationReason::Shutdown,
        CancellationReason::Preempted,
        CancellationReason::Superseded,
    ] {
        assert_eq!(
            validate_result_acceptance(&current, &current, JobStatus::Running, Some(reason)),
            Err(ContractError::ExecutionClosed)
        );
    }
}

#[test]
fn project_jobs_do_not_require_a_fake_pulsation() {
    let mut maintenance = fence();
    maintenance.pulsation_id = None;
    maintenance.input_revision = None;
    maintenance.validate().unwrap();
    maintenance.input_revision = Some(1);
    assert!(maintenance.validate().is_err());
}

#[test]
fn completed_output_does_not_skip_mandatory_human_review() {
    for stage in [
        PulsationStage::Specification,
        PulsationStage::Contract,
        PulsationStage::Test,
        PulsationStage::Integrator,
    ] {
        assert_eq!(
            stage_disposition(stage, &completed()).unwrap(),
            StageDisposition::AwaitArtifactReview
        );
    }
    assert_eq!(
        stage_disposition(PulsationStage::Intake, &completed()).unwrap(),
        StageDisposition::Advance(PulsationStage::Specification)
    );
    assert_eq!(
        stage_disposition(PulsationStage::Code, &completed()).unwrap(),
        StageDisposition::Advance(PulsationStage::Test)
    );
}

#[test]
fn failed_output_has_no_artifact_or_advance_channel() {
    let outcome = StageOutcome::Failed {
        code: "invalid_input".into(),
        message: "Entrada inválida.".into(),
    };
    assert_eq!(
        stage_disposition(PulsationStage::Code, &outcome).unwrap(),
        StageDisposition::Fail
    );
    assert!(
        serde_json::from_value::<StageOutcome>(
            json!({"outcome":"failed","code":"x","message":"x","artifacts":[]})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<StageOutcome>(json!({"outcome":"advance"})).is_err());
}

#[test]
fn only_specification_can_ask_nonempty_unique_questions() {
    let mut outcome = StageOutcome::NeedsClarification {
        questions: vec![question()],
    };
    assert_eq!(
        stage_disposition(PulsationStage::Specification, &outcome).unwrap(),
        StageDisposition::AwaitClarification
    );
    assert!(stage_disposition(PulsationStage::Contract, &outcome).is_err());
    outcome = StageOutcome::NeedsClarification { questions: vec![] };
    assert!(outcome.validate_for(PulsationStage::Specification).is_err());
    outcome = StageOutcome::NeedsClarification {
        questions: vec![question(), question()],
    };
    assert!(outcome.validate_for(PulsationStage::Specification).is_err());
}

#[test]
fn revision_targets_keep_specification_and_contract_in_their_own_stage() {
    assert_eq!(
        revision_target(PulsationStage::Specification),
        Some(PulsationStage::Specification)
    );
    assert_eq!(
        revision_target(PulsationStage::Contract),
        Some(PulsationStage::Contract)
    );
    assert_eq!(
        revision_target(PulsationStage::Test),
        Some(PulsationStage::Code)
    );
    assert_eq!(
        revision_target(PulsationStage::Integrator),
        Some(PulsationStage::Code)
    );
    assert_eq!(revision_target(PulsationStage::Intake), None);
}

#[test]
fn answers_require_the_exact_current_checkpoint_and_revision() {
    let pending = questions();
    validate_specification_answers(&answers(), &pending).unwrap();
    for dimension in 0..5 {
        let mut request = answers();
        match dimension {
            0 => request.checkpoint_id = "old-checkpoint".into(),
            1 => request.question_set_id = "old-set".into(),
            2 => request.expected_input_revision += 1,
            3 => request.expected_state_version += 1,
            _ => request.pulsation_id = "other-pulse".into(),
        }
        assert_eq!(
            validate_specification_answers(&request, &pending),
            Err(ContractError::StaleExecution)
        );
    }
}

#[test]
fn missing_unknown_duplicate_or_blank_answers_do_not_resume_specification() {
    let pending = questions();
    for invalid in [
        vec![],
        vec![SpecificationAnswer {
            question_id: "unknown".into(),
            answer: "x".into(),
        }],
        vec![SpecificationAnswer {
            question_id: "question-a".into(),
            answer: " ".into(),
        }],
        vec![answers().answers[0].clone(), answers().answers[0].clone()],
    ] {
        let mut request = answers();
        request.answers = invalid;
        assert!(validate_specification_answers(&request, &pending).is_err());
    }
}

#[test]
fn publication_replay_distinguishes_before_write_after_write_and_human_conflict() {
    let old = ContentSha256::of(b"old");
    let desired = ContentSha256::of(b"new");
    let human = ContentSha256::of(b"human edit");
    let expected = ExpectedFileContent::Present(old.clone());
    assert_eq!(
        publication_replay_decision(&expected, &desired, Some(&old)),
        PublicationReplayDecision::Apply
    );
    assert_eq!(
        publication_replay_decision(&expected, &desired, Some(&desired)),
        PublicationReplayDecision::AcknowledgeAlreadyApplied
    );
    assert_eq!(
        publication_replay_decision(&expected, &desired, Some(&human)),
        PublicationReplayDecision::Conflict
    );
    assert_eq!(
        publication_replay_decision(&expected, &desired, None),
        PublicationReplayDecision::Conflict
    );
    assert_eq!(
        publication_replay_decision(&ExpectedFileContent::Absent, &desired, None),
        PublicationReplayDecision::Apply
    );
    assert_eq!(
        publication_replay_decision(&ExpectedFileContent::Absent, &desired, Some(&old)),
        PublicationReplayDecision::Conflict
    );
}

#[test]
fn relative_paths_normalize_windows_separators_without_permitting_escape() {
    assert_eq!(
        RelativeSourcePath::new("src\\auth.rs").unwrap().as_str(),
        "src/auth.rs"
    );
    for path in [
        "",
        "/tmp/a",
        "../a",
        "a/../b",
        "a/./b",
        "C:\\a",
        "\\\\server\\a",
        "a//b",
        "a:stream",
        "a/ ",
        "a.",
        "a\0b",
    ] {
        assert!(RelativeSourcePath::new(path).is_err(), "accepted {path:?}");
    }
}

#[test]
fn source_hash_and_snippet_preserve_the_same_unicode_crlf_bytes() {
    let snapshot = source("project-a", "// segredo de fixture\r\nfn ação() {}\r\n");
    let snippet = snapshot.snippet(2, 2).unwrap();
    assert_eq!(snippet.content, "fn ação() {}\r\n");
    assert_eq!(
        snippet.source.content_sha256,
        ContentSha256::of(snapshot.content().as_bytes())
    );
    assert!(!format!("{snapshot:?}").contains("segredo de fixture"));
    assert!(snapshot.snippet(0, 1).is_err());
    assert!(snapshot.snippet(3, 3).is_err());
}

#[test]
fn sha256_encoding_is_validated_and_canonical() {
    let hash = ContentSha256::of(b"abc");
    assert_eq!(
        hash.as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let decoded: ContentSha256 =
        serde_json::from_value(json!(hash.as_str().to_uppercase())).unwrap();
    assert_eq!(decoded, hash);
    assert!(serde_json::from_value::<ContentSha256>(json!("invalid")).is_err());
}

#[test]
fn fact_identity_separates_project_content_and_extractor_versions() {
    let original = SourceFactKey {
        source: source("project-a", "fn check() { old() }\n")
            .revision()
            .clone(),
        extractor: extractor(),
    };
    let mut keys = HashSet::from([original.clone()]);
    for dimension in 0..5 {
        let mut changed = original.clone();
        match dimension {
            0 => changed.source.project_id = "project-b".into(),
            1 => changed.source.content_sha256 = ContentSha256::of(b"fn check() { new() }\n"),
            2 => changed.extractor.version = "2".into(),
            3 => changed.extractor.policy_version = "2".into(),
            _ => changed.extractor.resolution_context = Some("new-manifest".into()),
        }
        assert!(keys.insert(changed));
    }
    assert!(!keys.insert(original));
    assert_eq!(keys.len(), 6);
}

#[test]
fn context_detects_changed_bytes_and_fabricated_snippets() {
    let snapshot = source("project-a", "fn check() { old() }\n");
    let original = facts(&snapshot);
    original
        .validate_against(std::slice::from_ref(&snapshot))
        .unwrap();
    let changed = source("project-a", "fn check() { new() }\n");
    assert_eq!(
        original.validate_against(&[changed]),
        Err(ContractError::StaleExecution)
    );
    let mut fabricated = original;
    fabricated.snippets[0].content = "invented".into();
    assert!(fabricated.validate_against(&[snapshot]).is_err());
}

#[test]
fn partial_context_without_vault_is_valid_but_false_completeness_is_not() {
    let mut context = facts(&source("project-a", "fn f() {}\n"));
    context.validate().unwrap();
    context.coverage.state = CoverageState::Complete;
    context.coverage.truncated = true;
    assert!(context.validate().is_err());
    context.coverage.state = CoverageState::Unavailable;
    context.sources.clear();
    context.snippets.clear();
    context.validate().unwrap();
}

#[test]
fn unresolved_reference_does_not_imply_a_resolved_target() {
    let decoded: ReferenceResolution =
        serde_json::from_value(json!({"state":"unresolved","reason":"alias desconhecido"}))
            .unwrap();
    assert!(matches!(decoded, ReferenceResolution::Unresolved { .. }));
    assert!(serde_json::from_value::<ReferenceResolution>(json!({"state":"resolved"})).is_err());
}

#[test]
fn context_rejects_cross_project_evidence() {
    let mut context = facts(&source("project-a", "fn f() {}\n"));
    context.sources[0].project_id = "project-b".into();
    assert!(context.validate().is_err());
}

#[test]
fn prespec_budget_matches_the_accepted_v3_limits() {
    let budget = CollectionBudget::default();
    assert_eq!(budget.max_enumerations, 1);
    assert_eq!(budget.max_files, 2_000);
    assert_eq!(budget.max_read_bytes, 16 * 1024 * 1024);
    assert_eq!(budget.max_file_bytes, 1024 * 1024);
    assert_eq!(budget.max_package_files, 12);
    assert_eq!(budget.max_snippet_bytes, 32 * 1024);
    assert_eq!(budget.max_duration_ms, 2_000);
}

#[test]
fn handoff_round_trips_artifact_content_without_a_filesystem_path() {
    let original = handoff();
    let serialized = serde_json::to_value(&original).unwrap();
    assert!(serialized["artifacts"][0].get("relativePath").is_none());
    let decoded: StageHandoffV1 = serde_json::from_value(serialized).unwrap();
    assert_eq!(decoded, original);
    decoded.validate_for(&fence(), 1_000).unwrap();
}

#[test]
fn handoff_rejects_unaccepted_unpublished_corrupt_and_foreign_artifacts() {
    for dimension in 0..6 {
        let mut value = handoff();
        match dimension {
            0 => value.artifacts[0].acceptance = ArtifactAcceptance::Candidate,
            1 => value.artifacts[0].acceptance = ArtifactAcceptance::Superseded,
            2 => value.artifacts[0].publication = PublicationState::Pending,
            3 => value.artifacts[0].content.push_str("changed"),
            4 => value.artifacts[0].producer.pulsation_id = Some("other-pulse".into()),
            _ => value.artifacts[0].producer.input_revision = Some(2),
        }
        assert!(value.validate_for(&fence(), 1_000).is_err());
    }
}

#[test]
fn normative_content_is_never_silently_truncated_to_fit_the_budget() {
    let value = handoff();
    let original = value.artifacts[0].content.clone();
    assert_eq!(
        value.validate_for(&fence(), original.chars().count() - 1),
        Err(ContractError::BudgetExceeded)
    );
    assert_eq!(value.artifacts[0].content, original);
}

#[test]
fn unknown_envelope_version_fails_closed() {
    let mut value = serde_json::to_value(handoff()).unwrap();
    value["schemaVersion"] = json!(2);
    assert!(serde_json::from_value::<StageHandoffV1>(value).is_err());
}

#[test]
fn context_rejects_duplicate_sources_and_missing_extractor_version() {
    let original = facts(&source("project-a", "fn f() {}\n"));
    let mut duplicate = original.clone();
    duplicate.sources.push(duplicate.sources[0].clone());
    assert!(duplicate.validate().is_err());
    let mut mixed_revision = original.clone();
    mixed_revision.sources.push(
        source("project-a", "fn f() { changed() }\n")
            .revision()
            .clone(),
    );
    assert!(mixed_revision.validate().is_err());
    let captured = source("project-a", "fn f() {}\n");
    let mut bad_reference = original.clone();
    bad_reference.references.push(ObservedReference {
        source: captured.revision().clone(),
        line: 1,
        observed_text: "nonexistent_import".into(),
        resolution: ReferenceResolution::Unresolved {
            reason: "fixture".into(),
        },
    });
    assert!(bad_reference.validate_against(&[captured]).is_err());
    let mut unversioned = original;
    unversioned.extractor.version.clear();
    assert!(unversioned.validate().is_err());
}

#[test]
fn handoff_rejects_future_stages_and_duplicate_producer_positions() {
    let mut future = handoff();
    future.artifacts[0].producer_stage = PulsationStage::Test;
    assert!(future.validate_for(&fence(), 1_000).is_err());
    let mut duplicate = handoff();
    let mut second = duplicate.artifacts[0].clone();
    second.artifact_id = "different-id-same-output".into();
    duplicate.artifacts.push(second);
    assert!(duplicate.validate_for(&fence(), 1_000).is_err());
}

#[test]
fn collection_budget_rejects_zero_or_inconsistent_limits() {
    CollectionBudget::default().validate().unwrap();
    let empty = CollectionBudget {
        max_files: 0,
        ..CollectionBudget::default()
    };
    assert!(empty.validate().is_err());
    let inconsistent = CollectionBudget {
        max_read_bytes: 1,
        ..CollectionBudget::default()
    };
    assert!(inconsistent.validate().is_err());
}

#[test]
fn context_package_is_versioned_and_bound_to_the_current_request() {
    let mut value = handoff();
    value.context = Some(ContextPackageV1 {
        schema_version: SchemaV1,
        pulsation_id: "pulse-a".into(),
        input_revision: 1,
        ruleset_version: "1".into(),
        observed_base_epoch: 0,
        budget: CollectionBudget::default(),
        facts: facts(&source("project-a", "fn f() {}\n")),
        created_at: "2026-08-27T12:00:00Z".into(),
    });
    let decoded: StageHandoffV1 =
        serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
    decoded.validate_for(&fence(), 1_000).unwrap();
    value.context.as_mut().unwrap().input_revision = 2;
    assert_eq!(
        value.validate_for(&fence(), 1_000),
        Err(ContractError::StaleExecution)
    );
}
