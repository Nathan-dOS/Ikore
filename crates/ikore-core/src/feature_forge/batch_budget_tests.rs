use super::*;
use crate::feature_forge::{
    AcceptanceCriterion, Feature, FeatureBrief, FeatureEvaluation, FeatureState,
    PathConfirmationSource, TaskDraft, TaskKind, Wave, WorkTask,
};

fn manifest() -> BatchManifest {
    let draft = TaskDraft {
        title: "Document product vision".into(),
        kind: TaskKind::Foundation,
        description: "Record the approved product boundaries in docs/vision.md".into(),
        criteria: vec![AcceptanceCriterion {
            id: "criterion-1".into(),
            description: "The approved MVP and exclusions remain explicit.".into(),
        }],
        dependencies: Vec::new(),
        feature_id: Some("feature-1".into()),
    };
    BatchManifest {
        schema_version: 1,
        title: "Product foundation".into(),
        tasks: vec![WorkTask {
            id: "task-1".into(),
            project_id: "project-1".into(),
            revision: 1,
            draft: draft.clone(),
            feature_revision: Some(1),
            state: "ready".into(),
            active_pulsation_id: None,
            completed_pulsation_id: None,
        }],
        features: vec![Feature {
            id: "feature-1".into(),
            project_id: "project-1".into(),
            title: "Notes application".into(),
            original_idea: "Organize college work".into(),
            revision: 1,
            approved_revision: Some(1),
            state: FeatureState::Planned,
            brief: FeatureBrief {
                objective: "Keep the complete approved product objective.".into(),
                scope: "Define a local-first MVP.".into(),
                exclusions: "Defer account synchronization.".into(),
                decisions: vec!["Keep all approved decisions.".into()],
                criteria: draft.criteria.clone(),
                tasks: vec![draft],
                ..FeatureBrief::default()
            },
            evaluation: FeatureEvaluation::default(),
            structural_level: 0,
            wave: Some(Wave::Current),
            position: 0,
            archive_reason: None,
        }],
        feature_dependencies: Vec::new(),
        path_confirmation_source: PathConfirmationSource::ForgeGate,
        task_destinations: Vec::new(),
        destination_notices: Vec::new(),
    }
}

#[test]
fn a_single_task_is_not_charged_for_unselected_backlog_drafts() {
    let mut manifest = manifest();
    let base = assess_batch_context(&manifest, PipelineVersion::BATCH, 16_000).unwrap();
    let mut unselected = manifest.features[0].brief.tasks[0].clone();
    unselected.title = "Unselected backlog task".into();
    unselected.description = "Detailed future work. ".repeat(1_000);
    manifest.features[0].brief.tasks.push(unselected);
    let persisted = serde_json::to_string(&manifest).unwrap();
    assert!(persisted.chars().count() > 8_000);

    let actual = assess_batch_context(&manifest, PipelineVersion::BATCH, 16_000).unwrap();
    assert_eq!(actual, base);
    assert!(actual.fits());
    assert_eq!(manifest.features[0].brief.tasks.len(), 2);
    assert_eq!(serde_json::to_string(&manifest).unwrap(), persisted);
}

#[test]
fn prompt_projection_preserves_selected_work_and_the_full_approved_scope() {
    let manifest = manifest();
    let original = serde_json::to_value(&manifest).unwrap();
    let mut expected = original.clone();
    expected["features"][0]["brief"]["tasks"] = serde_json::json!([]);

    let projection = batch_prompt_projection(&manifest).unwrap();
    assert_eq!(projection, expected);
    assert_eq!(projection["tasks"], original["tasks"]);
    assert_eq!(serde_json::to_value(&manifest).unwrap(), original);
}

#[test]
fn admission_uses_the_pipeline_capacity_and_rejects_oversized_required_scope() {
    let mut manifest = manifest();
    manifest.features[0].brief.scope = "Required approved scope. ".repeat(500);
    let legacy = assess_batch_context(&manifest, PipelineVersion::BATCH, 16_000).unwrap();
    let composed =
        assess_batch_context(&manifest, PipelineVersion::COMPOSED_BATCH, 16_000).unwrap();
    assert_eq!(legacy.limit_chars, 8_000);
    assert_eq!(composed.limit_chars, 32_000);
    assert_eq!(legacy.required_chars, composed.required_chars);
    assert!(!legacy.fits());
    assert!(composed.fits());

    let required_scope = "Required approved scope. ".repeat(2_000);
    manifest.features[0].brief.scope = required_scope.clone();
    let oversized =
        assess_batch_context(&manifest, PipelineVersion::COMPOSED_BATCH, 16_000).unwrap();
    assert!(!oversized.fits());
    assert_eq!(manifest.features[0].brief.scope, required_scope);
}

#[test]
fn admission_counts_unicode_characters_and_includes_the_exact_limit() {
    let mut manifest = manifest();
    manifest.features[0].brief.scope = "Decisão acadêmica 🧭 ".repeat(300);
    let projected = serde_json::to_string(&batch_prompt_projection(&manifest).unwrap()).unwrap();
    let required = projected.chars().count();
    assert!(projected.len() > required);

    let at_limit = assess_batch_context(&manifest, PipelineVersion::BATCH, required * 2).unwrap();
    assert_eq!(at_limit.required_chars, required);
    assert_eq!(at_limit.limit_chars, required);
    assert!(at_limit.fits());
    let above = assess_batch_context(&manifest, PipelineVersion::BATCH, required * 2 - 1).unwrap();
    assert!(!above.fits());
    let below = assess_batch_context(&manifest, PipelineVersion::BATCH, required * 2 + 2).unwrap();
    assert!(below.fits());
}

#[test]
fn composed_admission_scales_with_the_existing_prompt_budget_policy() {
    let manifest = manifest();
    for (context_chars, expected_limit) in [(2_000, 32_000), (16_000, 32_000), (80_000, 160_000)] {
        let budget =
            assess_batch_context(&manifest, PipelineVersion::COMPOSED_BATCH, context_chars)
                .unwrap();
        assert_eq!(budget.limit_chars, expected_limit);
    }
}

#[test]
fn legacy_handoff_admits_unselected_backlog_but_still_bounds_approved_scope() {
    use crate::orchestration::{ContractError, ExecutionFence, SchemaV1, StageHandoffV1};
    use crate::persistence_service::PulsationStage;

    let mut manifest = manifest();
    let mut backlog = manifest.features[0].brief.tasks[0].clone();
    backlog.title = "Unselected backlog task".into();
    backlog.description = "Future backlog details. ".repeat(1_000);
    manifest.features[0].brief.tasks.push(backlog);
    assert!(serde_json::to_string(&manifest).unwrap().chars().count() > 16_000);
    let projected_chars = serde_json::to_string(&batch_prompt_projection(&manifest).unwrap())
        .unwrap()
        .chars()
        .count();
    let recipient = ExecutionFence {
        project_id: "project-1".into(),
        pulsation_id: Some("pulse-1".into()),
        input_revision: Some(1),
        job_id: "job-1".into(),
        generation: 1,
        state_version: 1,
    };
    let mut handoff = StageHandoffV1 {
        batch_manifest: Some(manifest),
        schema_version: SchemaV1,
        recipient: recipient.clone(),
        stage: PulsationStage::Intake,
        assessment: None,
        context: None,
        answers: Vec::new(),
        clarifications: Vec::new(),
        artifacts: Vec::new(),
        publication_bases: Vec::new(),
        revision_feedback: None,
    };
    let persisted = serde_json::to_string(&handoff).unwrap();
    assert_eq!(handoff.normative_chars(), projected_chars);
    handoff.validate_for(&recipient, 16_000).unwrap();
    assert!(
        !handoff
            .prompt_json()
            .unwrap()
            .contains("Unselected backlog task")
    );
    assert_eq!(serde_json::to_string(&handoff).unwrap(), persisted);

    handoff.batch_manifest.as_mut().unwrap().features[0]
        .brief
        .scope = "Required approved scope. ".repeat(1_000);
    assert_eq!(
        handoff.validate_for(&recipient, 16_000),
        Err(ContractError::BudgetExceeded)
    );
    assert!(
        handoff
            .prompt_json()
            .unwrap()
            .contains("Required approved scope.")
    );
}
