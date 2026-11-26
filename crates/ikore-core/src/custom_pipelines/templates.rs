//! Feature 27 (Camada A): built-in default global agent pipelines seeded on
//! initialization. They do not consume user global quotas and are immutable on
//! their own, though each project can activate, set its own trigger, archive,
//! run, or create an editable child pipeline from them.

use rusqlite::{Connection, TransactionBehavior, params};

use super::{PersistenceService, PersistenceServiceError};
use crate::custom_pipelines::{
    CUSTOM_PIPELINE_SCHEMA_VERSION, CanonicalPipelineRole, CanonicalStep,
    CustomPipelineDefinitionV1, DefaultEvent, DomainEvent, GateStep, PipelineStep, PipelineTrigger,
};

struct BuiltinTemplateSpec {
    id: &'static str,
    template_key: &'static str,
    name: &'static str,
    description: &'static str,
}

fn builtin_specs() -> Vec<(BuiltinTemplateSpec, CustomPipelineDefinitionV1)> {
    vec![
        (
            BuiltinTemplateSpec {
                id: "builtin-companion-notes",
                template_key: "builtin:companion-notes",
                name: "Companion notes",
                description: "Structures a newly captured note with Notes Scribe and holds it for review.",
            },
            CustomPipelineDefinitionV1 {
                schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
                trigger: PipelineTrigger::DomainEvent {
                    event: DomainEvent::NoteCaptured,
                    stage: None,
                },
                steps: vec![
                    PipelineStep::Canonical(CanonicalStep {
                        id: "structure-note".into(),
                        label: "Notes Scribe".into(),
                        role: CanonicalPipelineRole::NotesScribe,
                        instructions_append: String::new(),
                    }),
                    PipelineStep::Gate(GateStep {
                        id: "review-note".into(),
                        label: "Review structured note".into(),
                        prompt: "Review the structured note output and approve or reject.".into(),
                    }),
                ],
            },
        ),
        (
            BuiltinTemplateSpec {
                id: "builtin-docs-reconciliation",
                template_key: "builtin:docs-reconciliation",
                name: "Docs reconciliation",
                description: "Proposes an update to stale documentation when linked code changes.",
            },
            CustomPipelineDefinitionV1 {
                schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
                trigger: PipelineTrigger::DomainEvent {
                    event: DomainEvent::DocsStale,
                    stage: None,
                },
                steps: vec![
                    PipelineStep::Canonical(CanonicalStep {
                        id: "reconcile-docs".into(),
                        label: "Documentation Reconciler".into(),
                        role: CanonicalPipelineRole::DocumentationReconciler,
                        instructions_append: String::new(),
                    }),
                    PipelineStep::Gate(GateStep {
                        id: "review-docs".into(),
                        label: "Review doc update".into(),
                        prompt: "Review the proposed documentation update before closing.".into(),
                    }),
                ],
            },
        ),
        (
            BuiltinTemplateSpec {
                id: "builtin-checkpoint-visualizer",
                template_key: "builtin:checkpoint-visualizer",
                name: "Checkpoint review notes",
                description: "Summarizes review focus points when an Integrator checkpoint opens.",
            },
            CustomPipelineDefinitionV1 {
                schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
                trigger: PipelineTrigger::DomainEvent {
                    event: DomainEvent::CheckpointReady,
                    stage: Some("Integrator".into()),
                },
                steps: vec![PipelineStep::Canonical(CanonicalStep {
                    id: "visualize-checkpoint".into(),
                    label: "Visualizer".into(),
                    role: CanonicalPipelineRole::Visualizer,
                    instructions_append: String::new(),
                })],
            },
        ),
        (
            BuiltinTemplateSpec {
                id: "builtin-post-pulsation-learning",
                template_key: "builtin:post-pulsation-learning",
                name: "Post-pulsation learning",
                description: "Extracts context insights after a pulsation finishes and holds them for review.",
            },
            CustomPipelineDefinitionV1 {
                schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
                trigger: PipelineTrigger::DefaultEvent {
                    event: DefaultEvent::PulsationDone,
                    stage: None,
                },
                steps: vec![
                    PipelineStep::Canonical(CanonicalStep {
                        id: "extract-learning".into(),
                        label: "Learning agent".into(),
                        role: CanonicalPipelineRole::LearningAgent,
                        instructions_append: String::new(),
                    }),
                    PipelineStep::Gate(GateStep {
                        id: "review-learning".into(),
                        label: "Review context insights".into(),
                        prompt: "Review the extracted context insights and approve or reject."
                            .into(),
                    }),
                ],
            },
        ),
        (
            BuiltinTemplateSpec {
                id: "builtin-feature-handoff",
                template_key: "builtin:feature-handoff",
                name: "Feature handoff review",
                description: "Prepares an implementation readiness checklist after a feature is approved.",
            },
            CustomPipelineDefinitionV1 {
                schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
                trigger: PipelineTrigger::DomainEvent {
                    event: DomainEvent::FeatureApproved,
                    stage: None,
                },
                steps: vec![
                    PipelineStep::Canonical(CanonicalStep {
                        id: "feature-readiness".into(),
                        label: "Feature agent".into(),
                        role: CanonicalPipelineRole::FeatureAgent,
                        instructions_append: String::new(),
                    }),
                    PipelineStep::Gate(GateStep {
                        id: "confirm-handoff".into(),
                        label: "Confirm handoff notes".into(),
                        prompt: "Review the readiness checklist and approve to complete handoff."
                            .into(),
                    }),
                ],
            },
        ),
    ]
}

const SEED_TIMESTAMP: &str = "2026-11-26T00:00:00Z";

impl PersistenceService {
    /// Seeds the five built-in default global pipelines idempotently.
    pub(super) fn ensure_builtin_templates(
        db: &mut Connection,
    ) -> Result<(), PersistenceServiceError> {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (spec, definition) in builtin_specs() {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM custom_pipelines WHERE template_key=?1)",
                [spec.template_key],
                |row| row.get(0),
            )?;
            if exists {
                continue;
            }
            let json = serde_json::to_string(&definition)
                .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
            let sha = crate::exchange::render::sha256_hex(&json);
            tx.execute(
                "INSERT INTO custom_pipelines(
                   id, scope, project_id, name, description, current_version,
                   archived, created_at, updated_at, parent_pipeline_id, template_key
                 ) VALUES(?1, 'global', NULL, ?2, ?3, 1, 0, ?4, ?4, NULL, ?5)",
                params![
                    spec.id,
                    spec.name,
                    spec.description,
                    SEED_TIMESTAMP,
                    spec.template_key,
                ],
            )?;
            tx.execute(
                "INSERT INTO custom_pipeline_versions(
                   pipeline_id, version, definition_json, definition_sha256, created_at
                 ) VALUES(?1, 1, ?2, ?3, ?4)",
                params![spec.id, json, sha, SEED_TIMESTAMP],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
