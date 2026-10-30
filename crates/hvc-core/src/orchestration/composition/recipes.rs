//! Stage recipes v1 (`stage-composition-recipes-v1`), single-unit edition.
//!
//! Each stage prepares its exact inputs, proposes its work units, projects the
//! prompt of one unit, lowers a validated child into contributions and renders
//! the host documents. Everything here is pure: the host supplies the frozen
//! batch, the prior stages' typed payloads, the bytes it read and the settings;
//! the recipe never touches the database, the filesystem or a model.
//!
//! Intake, Specification, Contract and Integrator propose exactly one unit.
//! Code and Test plan their units from the approved proposals and bindings
//! (`planner`), falling back to one unit when the specification proposed
//! none. The zero-call paths (M5) plug into the same functions.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use hvc_contracts::exchange::{
    AgentProducer, ApprovalGate, ExchangeItemV1, ExchangeKind, HumanSource, Producer,
};
use hvc_contracts::stages::PulsationStage;
use serde::{Deserialize, Serialize};

pub use super::aggregate::ReconstructedArtifact;
use super::aggregate::{HostArtifact, UnitContribution};
use super::anchors::{DEFAULT_MAX_ANCHORS_PER_FILE, partition_file};
use super::edits::apply_destination_edits;
use super::plan::UnitDraft;
use super::planner::{Destination, PathRef, ProposalNode, plan_units};
use super::projection::{DeliveredSource, UnitPromptInputs};
use super::references::{ReferenceTable, ReferenceTableBuilder, slug, vault_note_path};
use super::render::{self, Frontmatter};
use super::review_boundary::{EvidenceClass, ReviewBoundary, SourceOrigin};
use super::review_policy::is_review_policy_path;
use super::validate::{ApprovedContext, ValidatedChild};
use super::{
    ArtifactKind, CheckSummaryV1, CompletedPayloadV1, CompositionError, ErrorCode, EvidenceItemV1,
    ExecutorKind, Handle, IntakePayloadV1, InterfaceContractV1, InterfaceGroupV1, InterfaceIntent,
    Invariant, MAX_REPLACE_FILE_BYTES, ProposalKind, ReferenceEntryV1, RelPath, Sha256Hex,
    SourceRole, SpecificationContractV1, Stage, StageExecutionPlanV1, TestPayloadV1,
    UnavailableReason, UnitModelResponseV1, UnitProposalV1, VerificationMode, WorkUnitSpecV1,
    scalar_len,
};
use crate::exchange::render::single_line;
use crate::feature_forge::BatchManifest;
use crate::orchestration::{SchemaV1, SpecificationQuestion, StageArtifactDraft};

pub const STAGE_PAYLOAD_KIND: &str = "stage_payload_v1";

/// Keys of the units a recipe defines itself; every other unit was planned
/// from an approved proposal.
const RECIPE_UNIT_KEYS: [&str; 6] = [
    "classify_batch",
    "specify_batch",
    "contract_batch",
    "implement_batch",
    "test_batch",
    "review_batch",
];

/// Whether a unit's objective is the text of an approved proposal rather
/// than the recipe's own (ADR-037: it is then agent output).
pub fn objective_from_proposal(unit: &WorkUnitSpecV1) -> bool {
    !RECIPE_UNIT_KEYS.contains(&unit.logical_unit_key.as_str())
}

/// The provenance line of a block the stage guidance embeds.
fn guidance_line(kind: ExchangeKind, producer: Producer, gates: &[ApprovalGate]) -> String {
    ExchangeItemV1::new(kind, producer)
        .approved(gates.iter().copied())
        .provenance_line()
}

/// Who wrote a delivered file, from where the host found it: a published
/// artifact of an earlier stage is agent output, approved at the first
/// checkpoint it passed before `reader`; anything else is the workspace.
pub fn source_provenance(origin: Option<&SourceOrigin>, reader: Stage) -> ExchangeItemV1 {
    match origin {
        Some(SourceOrigin::StageArtifact { stage, .. }) => {
            let produced = stage.to_pulsation();
            ExchangeItemV1::new(
                ExchangeKind::DeliveredFile,
                Producer::agent(AgentProducer::StageUnits(produced)),
            )
            .approved(ApprovalGate::first_checkpoint(
                produced,
                reader.to_pulsation(),
            ))
        }
        _ => ExchangeItemV1::new(ExchangeKind::DeliveredFile, Producer::Repository),
    }
}

/// The durable record of a stage's typed payload, persisted as a host
/// artifact so later stages resolve its handles through its own reference
/// table instead of guessing paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagePayloadRecord {
    pub schema_version: SchemaV1,
    pub stage: Stage,
    pub plan_sha256: super::Sha256Hex,
    pub references: Vec<ReferenceEntryV1>,
    /// The validated response of every unit of the stage, in plan order.
    pub units: Vec<UnitPayloadRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitPayloadRecord {
    pub logical_unit_key: Handle,
    pub response: UnitModelResponseV1,
}

impl StagePayloadRecord {
    /// The response of a single-unit stage (Intake, Specification, Contract).
    pub fn primary(&self) -> Option<&UnitModelResponseV1> {
        self.units.first().map(|unit| &unit.response)
    }

    pub fn responses(&self) -> impl Iterator<Item = &UnitModelResponseV1> {
        self.units.iter().map(|unit| &unit.response)
    }

    pub fn path_of(&self, handle: &Handle) -> Option<&RelPath> {
        self.references
            .iter()
            .find(|entry| entry.handle() == handle)
            .and_then(ReferenceEntryV1::relative_path)
    }

    pub fn role_of(&self, handle: &Handle) -> Option<SourceRole> {
        self.references
            .iter()
            .find(|entry| entry.handle() == handle)
            .and_then(ReferenceEntryV1::source_role)
    }
}

/// A file the host read from the fenced baseline or received from an earlier
/// stage's reconciled publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: RelPath,
    pub role: SourceRole,
    pub content: String,
    /// Where the host found it; the Integrator classifies its inputs by it.
    pub origin: SourceOrigin,
}

impl SourceFile {
    /// A baseline workspace file named by the batch brief.
    pub fn baseline(path: RelPath, content: impl Into<String>) -> Self {
        Self {
            role: role_for_path(&path),
            path,
            content: content.into(),
            origin: SourceOrigin::Brief,
        }
    }
}

/// Everything a recipe needs, gathered by the host.
#[derive(Debug, Clone, Default)]
pub struct StageInputs {
    pub title: String,
    pub manifest: Option<BatchManifest>,
    /// Typed payload records of earlier accepted stages, by stage.
    pub prior: BTreeMap<Stage, StagePayloadRecord>,
    /// Baseline files the host could read (brief sources, explicit task paths,
    /// earlier published artifacts). Deduplicated by the host.
    pub sources: Vec<SourceFile>,
    /// Explicit paths named by the batch that do not exist yet.
    pub missing_paths: Vec<RelPath>,
    /// Configured host checks (none until validation profiles exist).
    pub checks: Vec<Handle>,
    pub revision_feedback: Option<String>,
    pub clarification_answers: Vec<(String, String)>,
    /// Recipes the project promoted to plan several units (settings
    /// `qualifiedRecipes`); an unqualified Code/Test keeps one unit.
    pub qualified_recipes: Vec<String>,
    /// Delivered files granted to a unit of this stage at a failure
    /// checkpoint (ADR-035 M4), by logical unit key.
    pub scope_amendments: Vec<(String, RelPath)>,
}

impl StageInputs {
    fn plans_units(&self, stage: Stage) -> bool {
        self.qualified_recipes
            .iter()
            .any(|recipe| recipe == stage.as_str())
    }
}

/// The frozen preparation of one stage before dispatch.
#[derive(Clone)]
pub struct StagePreparation {
    pub table: ReferenceTable,
    pub drafts: Vec<UnitDraft>,
    pub approved: ApprovedContext,
    pub delivered: BTreeMap<Handle, String>,
    /// Where the host found each delivered file, for its provenance (ADR-037).
    pub origins: BTreeMap<Handle, SourceOrigin>,
    /// Model-facing derivatives. `delivered` remains the immutable edit base.
    pub source_redactions: BTreeMap<Handle, crate::secret_redactor::ContextRedaction>,
    pub sanitization_policy_identity: String,
    pub requirements: Vec<(Handle, String)>,
    pub decisions: Vec<String>,
    pub interfaces: Vec<InterfaceGroupV1>,
    pub extra_instructions: Option<String>,
    /// The Integrator's classified inputs (ADR-032), attached by the host
    /// after preparation; `None` for every other stage.
    pub review: Option<std::sync::Arc<ReviewBoundary>>,
}

impl std::fmt::Debug for StagePreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagePreparation")
            .field("sources", &self.delivered.len())
            .field("units", &self.drafts.len())
            .field(
                "sanitization_policy_identity",
                &self.sanitization_policy_identity,
            )
            .finish_non_exhaustive()
    }
}

/// Host-side outputs after the unit is validated.
#[derive(Debug, Clone)]
pub struct StageFinish<'a> {
    pub contributions: Vec<UnitContribution<'a>>,
    pub host_artifacts: Vec<HostArtifact>,
    pub payload_row: StageArtifactDraft,
    pub findings: Vec<String>,
    pub summary: String,
}

fn recipe_error(code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc006, code, message)
}

pub fn role_for_path(path: &RelPath) -> SourceRole {
    let lower = path.as_str().to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if lower.ends_with(".md") || lower.ends_with(".txt") || lower.ends_with(".rst") {
        SourceRole::Document
    } else if lower.starts_with("tests/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || name.ends_with("_test.rs")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".test.js")
        || name.ends_with("_test.py")
        || name.starts_with("test_")
    {
        SourceRole::Test
    } else if lower.ends_with(".toml")
        || lower.ends_with(".json")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".lock")
        || lower.ends_with(".cfg")
        || lower.ends_with(".ini")
    {
        SourceRole::Configuration
    } else {
        SourceRole::Source
    }
}

/// Deterministic candidate destination for authored tests when the approved
/// specification planned none: `tests/<slug>_test.<ext>` with the extension of
/// the first delivered source file.
pub fn default_test_destination(title: &str, sources: &[SourceFile]) -> Option<RelPath> {
    let extension = sources
        .iter()
        .filter(|source| source.role == SourceRole::Source)
        .filter_map(|source| source.path.as_str().rsplit('.').next())
        .find(|ext| {
            matches!(
                *ext,
                "rs" | "ts" | "tsx" | "js" | "py" | "go" | "java" | "cs"
            )
        })?;
    RelPath::new(format!(
        "tests/{}_test.{extension}",
        slug(title).replace('-', "_")
    ))
    .ok()
}

fn add_tasks(
    builder: &mut ReferenceTableBuilder,
    manifest: &BatchManifest,
) -> Result<Vec<Handle>, CompositionError> {
    let mut handles = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        let criteria = task
            .draft
            .criteria
            .iter()
            .map(|criterion| (criterion.id.clone(), criterion.description.clone()))
            .collect::<Vec<_>>();
        handles.push(builder.add_task(&task.id, task.revision, &task.draft.title, &criteria)?);
    }
    Ok(handles)
}

/// The task and criterion bindings of a frozen batch, with the same handles
/// `prepare` registers first for every stage. Readers that only hold a
/// persisted payload use it to label `task_*`/`crit_*` handles.
pub fn task_table(manifest: &BatchManifest) -> Result<ReferenceTable, CompositionError> {
    let mut builder = ReferenceTable::builder();
    add_tasks(&mut builder, manifest)?;
    Ok(builder.build())
}

fn all_criteria(table: &ReferenceTable) -> Vec<Handle> {
    table
        .scope()
        .iter()
        .flat_map(|scope| scope.criterion_refs.iter().cloned())
        .collect()
}

fn all_tasks(table: &ReferenceTable) -> Vec<Handle> {
    table
        .scope()
        .iter()
        .map(|scope| scope.task_ref.clone())
        .collect()
}

/// Adds every host-read source, returning `(handle, path)` pairs in input order.
fn add_sources(
    builder: &mut ReferenceTableBuilder,
    sources: &[SourceFile],
    delivered: &mut BTreeMap<Handle, String>,
    deliver: bool,
) -> Result<Vec<Handle>, CompositionError> {
    let mut handles = Vec::with_capacity(sources.len());
    for source in sources {
        let handle = builder.add_existing(
            source.path.clone(),
            super::Sha256Hex::of(source.content.as_bytes()),
            source.role,
        )?;
        if deliver {
            delivered.insert(handle.clone(), source.content.clone());
        }
        handles.push(handle);
    }
    Ok(handles)
}

/// The batch, the developer's feedback and the clarifications, each block
/// under its provenance line (ADR-037) and every entry on one line.
fn batch_guidance(manifest: &BatchManifest, inputs: &StageInputs) -> String {
    let mut text = guidance_line(
        ExchangeKind::FeatureBrief,
        Producer::agent(AgentProducer::ForgeBatch),
        &[ApprovalGate::ForgeBatch],
    );
    text.push('\n');
    for (position, task) in manifest.tasks.iter().enumerate() {
        text.push_str(&format!(
            "task_{}: {} ({:?}) - {}\n",
            position + 1,
            single_line(&task.draft.title),
            task.draft.kind,
            single_line(task.draft.description.trim())
        ));
    }
    for feature in &manifest.features {
        let brief = &feature.brief;
        text.push_str(&format!(
            "\nFeature `{}`: objective: {} behavior: {} scope: {} exclusions: {}\n",
            single_line(&feature.title),
            single_line(brief.objective.trim()),
            single_line(brief.behavior.trim()),
            single_line(brief.scope.trim()),
            single_line(brief.exclusions.trim())
        ));
        for decision in &brief.decisions {
            text.push_str(&format!("- decision: {}\n", single_line(decision)));
        }
        for pending in &brief.pending_decisions {
            text.push_str(&format!("- pending decision: {}\n", single_line(pending)));
        }
    }
    if let Some(feedback) = &inputs.revision_feedback {
        text.push_str(&format!(
            "\n{}\nRevision feedback from the developer: {}\n",
            guidance_line(
                ExchangeKind::RevisionFeedback,
                Producer::Human(HumanSource::CheckpointFeedback),
                &[]
            ),
            single_line(feedback.trim())
        ));
    }
    for (question, answer) in &inputs.clarification_answers {
        text.push_str(&format!(
            "\n{}\nClarification question: {}\n{}\nClarification answer: {}\n",
            guidance_line(
                ExchangeKind::ClarificationQuestion,
                Producer::agent(AgentProducer::SpecificationAuthor),
                &[]
            ),
            single_line(question.trim()),
            guidance_line(
                ExchangeKind::ClarificationAnswer,
                Producer::Human(HumanSource::ClarificationAnswer),
                &[]
            ),
            single_line(answer.trim())
        ));
    }
    text
}

/// Prepares a stage: reference table, drafts and prompt material.
pub fn prepare(stage: Stage, inputs: &StageInputs) -> Result<StagePreparation, CompositionError> {
    let manifest = inputs.manifest.as_ref().ok_or_else(|| {
        recipe_error(
            ErrorCode::HostContractError,
            "A v4 stage requires a frozen task batch.",
        )
    })?;
    let mut builder = ReferenceTable::builder();
    let tasks = add_tasks(&mut builder, manifest)?;
    for check in &inputs.checks {
        builder.add_check(check.clone())?;
    }
    let mut delivered = BTreeMap::new();
    let mut approved = ApprovedContext::default();
    let mut requirements = Vec::new();
    let mut decisions = Vec::new();
    let mut interfaces = Vec::new();
    let mut extra: Option<String>;
    let spec =
        inputs
            .prior
            .get(&Stage::Specification)
            .and_then(|record| match record.primary()? {
                UnitModelResponseV1::Completed {
                    payload: CompletedPayloadV1::Specification(spec),
                    ..
                } => Some((record, spec)),
                _ => None,
            });
    let contract =
        inputs
            .prior
            .get(&Stage::Contract)
            .and_then(|record| match record.primary()? {
                UnitModelResponseV1::Completed {
                    payload: CompletedPayloadV1::Contract(contract),
                    ..
                } => Some(contract),
                _ => None,
            });
    if let Some((_, spec)) = spec {
        for requirement in &spec.requirements {
            requirements.push((
                requirement.criterion_ref.clone(),
                requirement.requirement.clone(),
            ));
            if requirement.verification_mode == VerificationMode::NotApplicable {
                approved
                    .not_applicable_criteria
                    .push(requirement.criterion_ref.clone());
            }
        }
        decisions.extend(spec.decisions.iter().cloned());
        approved.proposal_keys = spec
            .unit_proposals
            .iter()
            .map(|p| p.unit_key.clone())
            .collect();
    }
    if let Some(contract) = contract {
        interfaces.extend(contract.groups.iter().cloned());
    }
    let drafts = match stage {
        Stage::Intake => {
            // Facts only: handles for the brief sources, no file content.
            let sources = add_sources(&mut builder, &inputs.sources, &mut delivered, false)?;
            extra = Some(batch_guidance(manifest, inputs));
            let table_criteria_count: usize =
                manifest.tasks.iter().map(|t| t.draft.criteria.len()).sum();
            vec![UnitDraft {
                logical_unit_key: Handle::new("classify_batch")
                    .expect("built-in recipe key is a valid handle"),
                executor_kind: ExecutorKind::IntakeClassifier,
                task_refs: tasks.clone(),
                criterion_refs: Vec::new(),
                depends_on: vec![],
                input_refs: sources,
                owned_destination_refs: vec![],
                required_check_refs: vec![],
                objective: format!(
                    "Classify the {} task(s) of this batch and assess their risk from the delivered facts.",
                    manifest.tasks.len()
                ),
                estimated_output_tokens: 150 * manifest.tasks.len() as u32
                    + 100
                    + table_criteria_count as u32,
                unit_id: None,
            }]
        }
        Stage::Specification => {
            let sources = add_sources(&mut builder, &inputs.sources, &mut delivered, true)?;
            let mut inputs_refs = sources;
            let mut planned_note = Vec::new();
            for missing in &inputs.missing_paths {
                planned_note.push(builder.add_planned(missing.clone(), role_for_path(missing))?);
            }
            if inputs.sources.is_empty() {
                inputs_refs.push(builder.add_unavailable(
                    UnavailableReason::Missing,
                    "No baseline source was delivered; the batch may be greenfield.",
                ));
            }
            if let Some(intake) =
                inputs
                    .prior
                    .get(&Stage::Intake)
                    .and_then(|record| match record.primary()? {
                        UnitModelResponseV1::Completed {
                            payload: CompletedPayloadV1::Intake(intake),
                            ..
                        } => Some(intake),
                        _ => None,
                    })
            {
                let mut text = batch_guidance(manifest, inputs);
                if !intake.issues.is_empty() {
                    text.push_str(&format!(
                        "\n{}\nIssues reported by Intake:\n",
                        guidance_line(
                            ExchangeKind::IntakeIssue,
                            Producer::agent(AgentProducer::IntakeClassifier),
                            &[]
                        )
                    ));
                    for issue in &intake.issues {
                        text.push_str(&format!("- {}\n", single_line(issue)));
                    }
                }
                extra = Some(text);
            } else {
                extra = Some(batch_guidance(manifest, inputs));
            }
            if !planned_note.is_empty() {
                extra = Some(format!(
                    "{}\nPlanned destinations available for new files: {}.\n",
                    extra.take().unwrap_or_default(),
                    planned_note
                        .iter()
                        .map(|h| format!("`{h}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                inputs_refs.extend(planned_note);
            }
            vec![UnitDraft {
                    logical_unit_key: Handle::new("specify_batch").expect("built-in recipe key is a valid handle"),
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    task_refs: tasks.clone(),
                    criterion_refs: all_criteria_of(&builder_scope_preview(manifest)),
                    depends_on: vec![],
                    input_refs: inputs_refs,
                    owned_destination_refs: vec![],
                    required_check_refs: vec![],
                    objective: "Write one specification for the batch: requirements per criterion, interface intent, files and cohesive unit proposals.".into(),
                    estimated_output_tokens: 400 + 250 * manifest.tasks.iter().map(|t| t.draft.criteria.len() as u32).sum::<u32>(),
                    unit_id: None,
            }]
        }
        Stage::Contract => {
            let Some((_, spec)) = spec else {
                return Err(recipe_error(
                    ErrorCode::HostContractError,
                    "Contract requires the approved specification payload.",
                ));
            };
            let sources = add_sources(&mut builder, &inputs.sources, &mut delivered, true)?;
            let mut inputs_refs = sources;
            if inputs.sources.is_empty() {
                inputs_refs.push(builder.add_unavailable(
                    UnavailableReason::Missing,
                    "No existing interface source was delivered.",
                ));
            }
            let mut text = batch_guidance(manifest, inputs);
            let specification = [ApprovalGate::Checkpoint(PulsationStage::Specification)];
            text.push_str(&format!(
                "\n{}\nApproved interface intent:\n",
                guidance_line(
                    ExchangeKind::InterfaceIntent,
                    Producer::agent(AgentProducer::SpecificationAuthor),
                    &specification
                )
            ));
            for intent in &spec.interface_intent {
                text.push_str(&format!(
                    "- {:?}: {}\n",
                    intent.intent,
                    single_line(&intent.description)
                ));
            }
            if !spec.unit_proposals.is_empty() {
                text.push_str(&format!(
                    "\n{}\nApproved unit proposals (bind each `unitKey` in `unitBindings`):\n",
                    guidance_line(
                        ExchangeKind::UnitProposal,
                        Producer::agent(AgentProducer::SpecificationAuthor),
                        &specification
                    )
                ));
                for proposal in &spec.unit_proposals {
                    text.push_str(&format!(
                        "- `{}` ({:?}): {}\n",
                        proposal.unit_key,
                        proposal.kind,
                        single_line(&proposal.objective)
                    ));
                }
            }
            extra = Some(text);
            vec![UnitDraft {
                    logical_unit_key: Handle::new("contract_batch").expect("built-in recipe key is a valid handle"),
                    executor_kind: ExecutorKind::ContractAuthor,
                    task_refs: tasks.clone(),
                    criterion_refs: all_criteria_of(&builder_scope_preview(manifest)),
                    depends_on: vec![],
                    input_refs: inputs_refs,
                    owned_destination_refs: vec![],
                    required_check_refs: vec![],
                    objective: "Define the new or changed interfaces the approved specification requires and bind the proposed units.".into(),
                    estimated_output_tokens: 300 + 300 * spec.interface_intent.len() as u32,
                    unit_id: None,
            }]
        }
        Stage::Code => {
            let Some((spec_record, spec)) = spec else {
                return Err(recipe_error(
                    ErrorCode::HostContractError,
                    "Code requires the approved specification payload.",
                ));
            };
            let sources = add_sources(&mut builder, &inputs.sources, &mut delivered, true)?;
            // Every explicit absent path that is not a test is a planned
            // destination of this stage, whether or not a proposal names it.
            let mut planned = Vec::new();
            for missing in &inputs.missing_paths {
                let role = role_for_path(missing);
                if role != SourceRole::Test {
                    planned.push(builder.add_planned(missing.clone(), role)?);
                }
            }
            extra = Some(batch_guidance(manifest, inputs));
            let proposals = spec
                .unit_proposals
                .iter()
                .filter(|proposal| proposal.kind == ProposalKind::Code)
                .collect::<Vec<_>>();
            if proposals.is_empty() || !inputs.plans_units(stage) {
                let mut owned = sources
                    .iter()
                    .zip(&inputs.sources)
                    .filter(|(_, source)| {
                        matches!(
                            source.role,
                            SourceRole::Source | SourceRole::Configuration | SourceRole::Document
                        )
                    })
                    .map(|(handle, _)| handle.clone())
                    .collect::<Vec<_>>();
                owned.extend(planned);
                if owned.is_empty() {
                    return Err(recipe_error(
                        ErrorCode::ScopeInsufficient,
                        "The approved specification names no existing or planned file for Code to own.",
                    ));
                }
                add_partition_anchors(&mut builder, &owned, &delivered)?;
                let mut inputs_refs = sources;
                let missing_inputs = owned
                    .iter()
                    .filter(|h| !inputs_refs.contains(h))
                    .cloned()
                    .collect::<Vec<_>>();
                inputs_refs.extend(missing_inputs);
                vec![UnitDraft {
                    logical_unit_key: Handle::new("implement_batch").expect("built-in recipe key is a valid handle"),
                    executor_kind: ExecutorKind::CodeImplementer,
                    task_refs: tasks.clone(),
                    criterion_refs: all_criteria_of(&builder_scope_preview(manifest)),
                    depends_on: vec![],
                    input_refs: inputs_refs,
                    owned_destination_refs: owned.clone(),
                    required_check_refs: inputs.checks.clone(),
                    objective: "Implement the approved specification against the approved interfaces in the owned files.".into(),
                    estimated_output_tokens: 1_024 + 512 * owned.len() as u32,
                    unit_id: None,
                }]
            } else {
                plan_stage_units(
                    &mut builder,
                    &delivered,
                    inputs,
                    spec_record,
                    &proposals,
                    contract,
                    ExecutorKind::CodeImplementer,
                    SourceRole::Test,
                    &[],
                )?
            }
        }
        Stage::Test => {
            let Some((spec_record, spec)) = spec else {
                return Err(recipe_error(
                    ErrorCode::HostContractError,
                    "Test requires the approved specification payload.",
                ));
            };
            let sources = add_sources(&mut builder, &inputs.sources, &mut delivered, true)?;
            let mut planned = Vec::new();
            for missing in &inputs.missing_paths {
                if role_for_path(missing) == SourceRole::Test {
                    planned.push(builder.add_planned(missing.clone(), SourceRole::Test)?);
                }
            }
            extra = Some(batch_guidance(manifest, inputs));
            let proposals = spec
                .unit_proposals
                .iter()
                .filter(|proposal| proposal.kind == ProposalKind::Test)
                .collect::<Vec<_>>();
            if proposals.is_empty() || !inputs.plans_units(stage) {
                let mut owned = sources
                    .iter()
                    .zip(&inputs.sources)
                    .filter(|(_, source)| source.role == SourceRole::Test)
                    .map(|(handle, _)| handle.clone())
                    .collect::<Vec<_>>();
                owned.extend(planned);
                if owned.is_empty()
                    && let Some(destination) =
                        default_test_destination(&inputs.title, &inputs.sources)
                {
                    owned.push(builder.add_planned(destination, SourceRole::Test)?);
                }
                add_partition_anchors(&mut builder, &owned, &delivered)?;
                let mut inputs_refs = sources;
                let missing_inputs = owned
                    .iter()
                    .filter(|h| !inputs_refs.contains(h))
                    .cloned()
                    .collect::<Vec<_>>();
                inputs_refs.extend(missing_inputs);
                vec![UnitDraft {
                    logical_unit_key: Handle::new("test_batch").expect("built-in recipe key is a valid handle"),
                    executor_kind: ExecutorKind::TestAuthor,
                    task_refs: tasks.clone(),
                    criterion_refs: all_criteria_of(&builder_scope_preview(manifest)),
                    depends_on: vec![],
                    input_refs: inputs_refs,
                    owned_destination_refs: owned.clone(),
                    required_check_refs: inputs.checks.clone(),
                    objective: "Author the tests that prove every criterion against the accepted production candidate and declare coverage.".into(),
                    estimated_output_tokens: 1_500 + 600 * manifest.tasks.iter().map(|t| t.draft.criteria.len() as u32).sum::<u32>(),
                    unit_id: None,
                }]
            } else {
                // The accepted Code aggregate is re-read by the host before
                // Test preparation, so every readable production source is a
                // delivered candidate input even when a model proposal omitted
                // or malformed its `inputRefs`.
                let production_inputs = inputs
                    .sources
                    .iter()
                    .filter(|source| source.role != SourceRole::Test)
                    .filter_map(|source| builder.handle_for_path(&source.path).cloned())
                    .collect::<Vec<_>>();
                plan_stage_units(
                    &mut builder,
                    &delivered,
                    inputs,
                    spec_record,
                    &proposals,
                    contract,
                    ExecutorKind::TestAuthor,
                    SourceRole::Source,
                    &production_inputs,
                )?
            }
        }
        Stage::Integrator => {
            // ADR-032: stage documents (the Specification note, the Test
            // report) are not delivered. Their normative content arrives
            // typed, and the Test report mixes the author's narrative with
            // the host's checks. The review policy has its own channel.
            let reviewed = inputs
                .sources
                .iter()
                .filter(|source| {
                    source.origin.class() != EvidenceClass::StageDocument
                        && !is_review_policy_path(source.path.as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            let sources = add_sources(&mut builder, &reviewed, &mut delivered, true)?;
            // Only baseline documents stay editable: the reviewer never
            // edits the candidate it reviews.
            let owned = sources
                .iter()
                .zip(&reviewed)
                .filter(|(_, source)| {
                    source.role == SourceRole::Document
                        && source.origin.class() == EvidenceClass::BaselineContext
                })
                .map(|(handle, _)| handle.clone())
                .collect::<Vec<_>>();
            extra = Some(batch_guidance(manifest, inputs));
            vec![UnitDraft {
                    logical_unit_key: Handle::new("review_batch").expect("built-in recipe key is a valid handle"),
                    executor_kind: ExecutorKind::IntegrationReviewer,
                    task_refs: tasks.clone(),
                    criterion_refs: all_criteria_of(&builder_scope_preview(manifest)),
                    depends_on: vec![],
                    input_refs: sources,
                    owned_destination_refs: owned,
                    required_check_refs: inputs.checks.clone(),
                    objective: "Assess every criterion against the final artifacts, contracts and host check results.".into(),
                    estimated_output_tokens: 300 + 200 * manifest.tasks.iter().map(|t| t.draft.criteria.len() as u32).sum::<u32>(),
                    unit_id: None,
            }]
        }
    };
    let mut drafts = drafts;
    // ADR-035 M4: a granted file is one more destination of the unit that
    // asked for it, with the anchors its edits need.
    if matches!(stage, Stage::Code | Stage::Test) {
        let mut granted = Vec::new();
        for (unit_key, path) in &inputs.scope_amendments {
            let Some(handle) = builder.handle_for_path(path).cloned() else {
                continue;
            };
            if let Some(draft) = drafts
                .iter_mut()
                .find(|draft| draft.logical_unit_key.as_str() == unit_key)
                && !draft.owned_destination_refs.contains(&handle)
            {
                draft.owned_destination_refs.push(handle.clone());
                if !draft.input_refs.contains(&handle) {
                    draft.input_refs.push(handle.clone());
                }
                granted.push(handle);
            }
        }
        add_partition_anchors(&mut builder, &granted, &delivered)?;
    }
    let origins = inputs
        .sources
        .iter()
        .filter_map(|source| {
            let handle = builder.handle_for_path(&source.path)?;
            delivered
                .contains_key(handle)
                .then(|| (handle.clone(), source.origin.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let table = builder.build();
    for draft in &mut drafts {
        // A review policy is never a destination of any stage (ADR-032).
        draft.owned_destination_refs.retain(|handle| {
            table
                .entry(handle)
                .and_then(ReferenceEntryV1::relative_path)
                .is_none_or(|path| !is_review_policy_path(path.as_str()))
        });
        if draft.criterion_refs.is_empty() {
            draft.criterion_refs = all_criteria(&table);
        }
        if draft.task_refs.is_empty() {
            draft.task_refs = all_tasks(&table);
        }
    }
    let source_redactions = delivered
        .iter()
        .map(|(handle, content)| {
            crate::secret_redactor::redact_context(content)
                .map(|view| (handle.clone(), view))
                .map_err(|_| {
                    recipe_error(
                        ErrorCode::HostContractError,
                        "Context sanitization is unavailable.",
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(StagePreparation {
        table,
        drafts,
        approved,
        delivered,
        origins,
        source_redactions,
        sanitization_policy_identity: crate::secret_redactor::SANITIZATION_POLICY_VERSION
            .to_owned(),
        requirements,
        decisions,
        interfaces,
        extra_instructions: extra,
        review: None,
    })
}

/// Issues exact anchors for every delivered file a unit may edit, cut along
/// the file's structure (symbols, headings or blocks) and capped so the plan
/// stays within its 256-anchor admission limit.
fn add_partition_anchors(
    builder: &mut ReferenceTableBuilder,
    owned: &[Handle],
    delivered: &BTreeMap<Handle, String>,
) -> Result<(), CompositionError> {
    let files = owned
        .iter()
        .filter(|handle| {
            delivered
                .get(handle)
                .is_some_and(|content| !content.is_empty())
        })
        .count()
        .max(1);
    let cap = (240 / files).clamp(1, DEFAULT_MAX_ANCHORS_PER_FILE);
    for handle in owned {
        let Some(content) = delivered.get(handle).filter(|content| !content.is_empty()) else {
            continue;
        };
        let Some(path) = builder
            .entry(handle)
            .and_then(ReferenceEntryV1::relative_path)
            .cloned()
        else {
            continue;
        };
        for (start, end) in partition_file(&path, content, cap).intervals {
            builder.add_anchor(handle, content.as_bytes(), start, end)?;
        }
    }
    Ok(())
}

/// Plans the Code/Test units of a stage from the approved proposals of the
/// matching kind: resolves the Specification-stage handles to this stage's
/// table by path, refines dependencies with the Contract bindings, groups
/// through `planner::plan_units`, registers candidate reads and anchors.
#[allow(clippy::too_many_arguments)]
fn plan_stage_units(
    builder: &mut ReferenceTableBuilder,
    delivered: &BTreeMap<Handle, String>,
    inputs: &StageInputs,
    spec_record: &StagePayloadRecord,
    proposals: &[&UnitProposalV1],
    contract: Option<&InterfaceContractV1>,
    kind: ExecutorKind,
    forbidden_role: SourceRole,
    fallback_inputs: &[Handle],
) -> Result<Vec<UnitDraft>, CompositionError> {
    let stage_name = kind.stage().as_str();
    // A Specification-stage handle names a path; the same path has a
    // (possibly different) handle in this stage's table.
    let resolve = |handle: &Handle| -> Option<(Handle, RelPath)> {
        let path = spec_record.path_of(handle)?;
        let current = builder.handle_for_path(path)?;
        Some((current.clone(), path.clone()))
    };
    let mut nodes = Vec::with_capacity(proposals.len());
    for proposal in proposals {
        let mut destinations = Vec::new();
        for handle in &proposal.destination_refs {
            let Some((current, path)) = resolve(handle) else {
                return Err(recipe_error(
                    ErrorCode::ScopeInsufficient,
                    format!(
                        "Proposal `{}` writes `{handle}`, which is neither delivered nor planned for {stage_name}.",
                        proposal.unit_key
                    ),
                )
                .with_related([proposal.unit_key.clone()]));
            };
            let role = builder
                .entry(&current)
                .and_then(ReferenceEntryV1::source_role)
                .unwrap_or_else(|| role_for_path(&path));
            if role == forbidden_role {
                return Err(recipe_error(
                    ErrorCode::OwnershipViolation,
                    format!(
                        "Proposal `{}` writes `{path}`, a {} file, outside the {stage_name} stage's ownership.",
                        proposal.unit_key,
                        match forbidden_role {
                            SourceRole::Test => "test",
                            _ => "production",
                        }
                    ),
                )
                .with_related([proposal.unit_key.clone()]));
            }
            // A file the approved Specification planned keeps its planned
            // estimate after this stage published it, so a revision or Retry
            // groups the units exactly like the first run. Sizing it by its
            // published bytes regrouped a revised Code ("Teste Haiku Only 1",
            // 2026-09-24). The delivered content itself is unchanged.
            let planned_at_specification = spec_record.references.iter().any(|entry| {
                entry.handle() == handle
                    && matches!(entry, ReferenceEntryV1::PlannedDestination { .. })
            });
            destinations.push(Destination {
                handle: current.clone(),
                path,
                role,
                baseline_bytes: if planned_at_specification {
                    None
                } else {
                    delivered.get(&current).map(String::len)
                },
            });
        }
        let mut node_inputs = Vec::new();
        for handle in &proposal.input_refs {
            let Some((current, path)) = resolve(handle) else {
                return Err(recipe_error(
                    ErrorCode::StaleInput,
                    format!(
                        "Proposal `{}` reads `{handle}`, which is no longer delivered to {stage_name}.",
                        proposal.unit_key
                    ),
                )
                .with_related([proposal.unit_key.clone()]));
            };
            // A planned file another proposal writes stays an input: the
            // planner turns it into a dependency and delivers the validated
            // candidate. Dropping it left greenfield units blind to their
            // predecessors' files.
            let written_by_proposal = proposals.iter().any(|other| {
                other.unit_key != proposal.unit_key
                    && other
                        .destination_refs
                        .iter()
                        .any(|destination| spec_record.path_of(destination) == Some(&path))
            });
            if builder
                .entry(&current)
                .is_some_and(ReferenceEntryV1::is_readable_input)
                || written_by_proposal
            {
                node_inputs.push(PathRef {
                    handle: current,
                    path,
                });
            }
        }
        let mut depends_on = proposal.depends_on.clone();
        if let Some(binding) = contract.and_then(|contract| {
            contract
                .unit_bindings
                .iter()
                .find(|binding| binding.unit_key == proposal.unit_key)
        }) {
            for dependency in &binding.depends_on {
                if !depends_on.contains(dependency) {
                    depends_on.push(dependency.clone());
                }
            }
        }
        nodes.push(ProposalNode {
            key: proposal.unit_key.clone(),
            task_refs: proposal.task_refs.clone(),
            criterion_refs: proposal.criterion_refs.clone(),
            inputs: node_inputs,
            destinations,
            depends_on,
            objective: proposal.objective.clone(),
        });
    }
    let planned = plan_units(&nodes, builder.scope())?;
    let mut owned_all = Vec::new();
    let mut drafts = Vec::with_capacity(planned.len());
    let mut candidate_handles: BTreeMap<String, (super::Uuid, Option<Handle>, SourceRole, Handle)> =
        BTreeMap::new();
    for unit in planned {
        let mut input_refs = unit.input_refs.clone();
        for read in &unit.candidate_reads {
            // The frozen digest is the baseline the producer starts from; the
            // digest of the delivered candidate is bound on the attempt.
            let baseline = read
                .baseline_handle
                .as_ref()
                .and_then(|handle| builder.entry(handle))
                .and_then(ReferenceEntryV1::content_sha256)
                .cloned()
                .unwrap_or_else(|| Sha256Hex::of(b""));
            let path_key = read.path.canonical_key();
            let candidate = if let Some((producer, baseline_handle, role, handle)) =
                candidate_handles.get(&path_key)
            {
                if producer != &read.producer_unit_id
                    || baseline_handle != &read.baseline_handle
                    || role != &read.role
                {
                    return Err(recipe_error(
                        ErrorCode::ReferenceConflict,
                        format!(
                            "Candidate read `{}` resolves to conflicting producer or baseline identities.",
                            read.path
                        ),
                    ));
                }
                handle.clone()
            } else {
                let handle = builder.add_candidate(
                    read.path.clone(),
                    baseline,
                    read.producer_unit_id.clone(),
                    read.role,
                )?;
                candidate_handles.insert(
                    path_key,
                    (
                        read.producer_unit_id.clone(),
                        read.baseline_handle.clone(),
                        read.role,
                        handle.clone(),
                    ),
                );
                handle
            };
            if !input_refs.contains(&candidate) {
                input_refs.push(candidate);
            }
        }
        if kind == ExecutorKind::TestAuthor {
            for handle in fallback_inputs {
                if !input_refs.contains(handle) {
                    input_refs.push(handle.clone());
                }
            }
            if input_refs.is_empty() {
                return Err(recipe_error(
                    ErrorCode::ScopeInsufficient,
                    format!(
                        "Test unit `{}` has no readable production candidate; dispatch is refused.",
                        unit.key
                    ),
                )
                .with_related([unit.key.clone()]));
            }
        }
        let mut owned_destination_refs = unit.owned_destination_refs;
        if kind == ExecutorKind::TestAuthor && owned_destination_refs.is_empty() {
            let destination = default_test_destination(&inputs.title, &inputs.sources).ok_or_else(|| {
                recipe_error(
                    ErrorCode::ScopeInsufficient,
                    format!(
                        "Test unit `{}` has no owned test destination and the host cannot derive one.",
                        unit.key
                    ),
                )
                .with_related([unit.key.clone()])
            })?;
            let handle = match builder.handle_for_path(&destination) {
                Some(handle) => handle.clone(),
                None => builder.add_planned(destination, SourceRole::Test)?,
            };
            owned_destination_refs.push(handle);
        }
        owned_all.extend(owned_destination_refs.iter().cloned());
        drafts.push(UnitDraft {
            logical_unit_key: unit.key,
            executor_kind: kind,
            task_refs: unit.task_refs,
            criterion_refs: unit.criterion_refs,
            depends_on: unit.depends_on,
            input_refs,
            owned_destination_refs,
            required_check_refs: inputs.checks.clone(),
            objective: unit.objective,
            estimated_output_tokens: unit.estimated_output_tokens,
            unit_id: Some(unit.unit_id),
        });
    }
    add_partition_anchors(builder, &owned_all, delivered)?;
    Ok(drafts)
}

/// Criterion handles as the builder will assign them (deterministic from the
/// manifest), usable before `build()`.
fn builder_scope_preview(manifest: &BatchManifest) -> Vec<Handle> {
    let mut handles = Vec::new();
    for (task_index, task) in manifest.tasks.iter().enumerate() {
        for criterion_index in 0..task.draft.criteria.len() {
            handles.push(
                Handle::new(format!("crit_{}_{}", task_index + 1, criterion_index + 1))
                    .expect("generated handle"),
            );
        }
    }
    handles
}

fn all_criteria_of(handles: &[Handle]) -> Vec<Handle> {
    handles.to_vec()
}

/// Prompt material for one unit of a prepared stage.
pub fn prompt_inputs<'a>(
    preparation: &'a StagePreparation,
    unit: &'a WorkUnitSpecV1,
) -> UnitPromptInputs<'a> {
    let delivered = unit
        .input_refs
        .iter()
        .filter_map(|handle| {
            let content = preparation.delivered.get(handle)?;
            Some(DeliveredSource {
                handle,
                content,
                redaction: preparation.source_redactions.get(handle),
                replaceable: unit.owned_destination_refs.contains(handle)
                    && content.len() <= MAX_REPLACE_FILE_BYTES
                    && preparation
                        .source_redactions
                        .get(handle)
                        .is_some_and(|view| view.spans.is_empty()),
                provenance: source_provenance(preparation.origins.get(handle), unit.stage),
            })
        })
        .collect();
    UnitPromptInputs {
        requirements: preparation
            .requirements
            .iter()
            .filter(|(criterion, _)| unit.criterion_refs.contains(criterion))
            .map(|(criterion, text)| (criterion, text.as_str()))
            .collect(),
        not_applicable: preparation
            .approved
            .not_applicable_criteria
            .iter()
            .filter(|criterion| unit.criterion_refs.contains(criterion))
            .collect(),
        decisions: &preparation.decisions,
        interfaces: &preparation.interfaces,
        delivered,
        dependency_outputs: Vec::new(),
        correction: None,
        extra_instructions: preparation.extra_instructions.as_deref(),
        review: preparation.review.as_deref(),
    }
}

/// Lowers `needs_input` questions into the legacy clarification shape with
/// host-assigned identifiers.
pub fn clarification_questions(questions: &[super::QuestionV1]) -> Vec<SpecificationQuestion> {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| SpecificationQuestion {
            id: format!("q_{}", index + 1),
            question: format!(
                "{} (tasks: {})",
                question.question.trim(),
                question
                    .task_refs
                    .iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            rationale: question.rationale.clone(),
        })
        .collect()
}

/// Reconstructs every destination a validated child edited.
pub fn reconstruct_artifacts(
    plan: &StageExecutionPlanV1,
    preparation: &StagePreparation,
    child: &ValidatedChild<'_>,
) -> Result<Vec<ReconstructedArtifact>, CompositionError> {
    let mut artifacts = Vec::with_capacity(child.destinations.len());
    for group in &child.destinations {
        let entry = preparation.table.entry(&group.destination).ok_or_else(|| {
            recipe_error(
                ErrorCode::ReferenceConflict,
                format!("Destination `{}` is unknown.", group.destination),
            )
        })?;
        let original = preparation
            .delivered
            .get(&group.destination)
            .map(String::as_bytes);
        let anchors = plan
            .anchors
            .iter()
            .filter(|anchor| anchor.source_ref == group.destination)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(view) = preparation.source_redactions.get(&group.destination) {
            super::edits::guard_redacted_edits(original, &view.spans, &anchors, &group.operations)?;
        } else if original.is_some() {
            return Err(recipe_error(
                ErrorCode::HostContractError,
                "The edit base has no sanitization map.",
            ));
        }
        let reconstructed = apply_destination_edits(original, &anchors, &group.operations)?;
        let path = entry.relative_path().cloned();
        let kind = match entry.source_role() {
            Some(SourceRole::Test) => ArtifactKind::Test,
            Some(SourceRole::Document) => ArtifactKind::Documentation,
            _ => ArtifactKind::Code,
        };
        artifacts.push(ReconstructedArtifact {
            artifact_key: group.artifact_key.clone(),
            relative_path: path,
            kind,
            content: reconstructed.content,
        });
    }
    Ok(artifacts)
}

fn payload_row(
    stage: Stage,
    plan: &StageExecutionPlanV1,
    table: &ReferenceTable,
    units: &[(&WorkUnitSpecV1, &ValidatedChild<'_>)],
) -> Result<StageArtifactDraft, CompositionError> {
    let record = StagePayloadRecord {
        schema_version: SchemaV1,
        stage,
        plan_sha256: plan.plan_sha256.clone(),
        references: table.entries().to_vec(),
        units: units
            .iter()
            .map(|(unit, child)| UnitPayloadRecord {
                logical_unit_key: unit.logical_unit_key.clone(),
                response: child.response.clone(),
            })
            .collect(),
    };
    Ok(StageArtifactDraft {
        name: format!("{} payload", stage.as_str()),
        kind: STAGE_PAYLOAD_KIND.into(),
        relative_path: None,
        content: serde_json::to_string(&record)
            .map_err(|error| recipe_error(ErrorCode::HostContractError, error.to_string()))?,
    })
}

fn cited_file_refs(
    table: &ReferenceTable,
    handles: impl IntoIterator<Item = Handle>,
) -> Vec<Handle> {
    handles
        .into_iter()
        .filter(|handle| {
            table
                .entry(handle)
                .is_some_and(|entry| entry.relative_path().is_some())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Synthesizes one evidence item per task for stages whose payload carries no
/// evidence array (Intake, Specification, Contract, Integrator).
fn host_evidence(
    table: &ReferenceTable,
    artifact_key: &Handle,
    explanation_for: impl Fn(&Handle) -> String,
) -> Vec<EvidenceItemV1> {
    table
        .scope()
        .iter()
        .map(|scope| EvidenceItemV1 {
            task_ref: scope.task_ref.clone(),
            criterion_refs: scope.criterion_refs.clone(),
            artifact_keys: vec![artifact_key.clone()],
            input_refs: vec![],
            explanation: {
                let text = explanation_for(&scope.task_ref);
                if scalar_len(&text) > 4_000 {
                    text.chars().take(3_997).collect::<String>() + "..."
                } else if text.trim().is_empty() {
                    "Represented in the host-rendered stage document.".into()
                } else {
                    text
                }
            },
        })
        .collect()
}

/// Lowers every validated unit of a stage into contributions, renders the
/// stage documents once from all of them and builds the payload row.
pub fn finish_stage<'a>(
    stage: Stage,
    inputs: &StageInputs,
    plan: &StageExecutionPlanV1,
    preparation: &StagePreparation,
    units: &[(&'a WorkUnitSpecV1, &ValidatedChild<'_>)],
    checks: &[CheckSummaryV1],
) -> Result<StageFinish<'a>, CompositionError> {
    let table = &preparation.table;
    let title = inputs.title.as_str();
    let mut payloads = Vec::with_capacity(units.len());
    for (unit, child) in units {
        let UnitModelResponseV1::Completed { payload, .. } = child.response else {
            return Err(recipe_error(
                ErrorCode::HostContractError,
                format!(
                    "Unit `{}` has no completed response to aggregate.",
                    unit.logical_unit_key
                ),
            ));
        };
        payloads.push((*unit, *child, payload));
    }
    let payload_row = payload_row(stage, plan, table, units)?;
    let mut host_artifacts = Vec::new();
    let mut findings = Vec::new();
    let mut contributions = Vec::with_capacity(units.len());
    let single = |what: &str| {
        recipe_error(
            ErrorCode::HostContractError,
            format!("{what} is produced by exactly one unit per stage."),
        )
    };
    let summary = match stage {
        Stage::Intake => {
            let mut tasks = Vec::new();
            let mut issues = Vec::new();
            for (_, _, payload) in &payloads {
                let CompletedPayloadV1::Intake(intake) = payload else {
                    return Err(single("Intake classification"));
                };
                tasks.extend(intake.tasks.iter().cloned());
                issues.extend(intake.issues.iter().cloned());
            }
            let merged = IntakePayloadV1 {
                tasks,
                issues,
                used_refs: Vec::new(),
            };
            let key = Handle::new("intake_note").expect("built-in recipe key is a valid handle");
            host_artifacts.push(HostArtifact {
                artifact_key: key.clone(),
                relative_path: None,
                kind: ArtifactKind::Intake,
                content: render::render_intake_note(title, &merged, table),
            });
            findings.extend(merged.issues.iter().cloned());
            let summaries: HashMap<&Handle, &String> = merged
                .tasks
                .iter()
                .map(|t| (&t.task_ref, &t.summary))
                .collect();
            contributions.push(UnitContribution {
                unit: payloads[0].0,
                evidence: host_evidence(table, &key, |task| {
                    summaries
                        .get(task)
                        .map(|s| s.to_string())
                        .unwrap_or_default()
                }),
                artifacts: Vec::new(),
            });
            format!("Intake classified {} task(s).", merged.tasks.len())
        }
        Stage::Specification => {
            let [(unit, _, CompletedPayloadV1::Specification(spec))] = payloads.as_slice() else {
                return Err(single("The specification"));
            };
            let key =
                Handle::new("specification_note").expect("built-in recipe key is a valid handle");
            let cited = cited_file_refs(
                table,
                spec.existing_file_refs
                    .iter()
                    .chain(spec.planned_file_refs.iter())
                    .cloned(),
            );
            let frontmatter = Frontmatter::from_references(table, &cited);
            host_artifacts.push(HostArtifact {
                artifact_key: key.clone(),
                relative_path: Some(vault_note_path("features", title)?),
                kind: ArtifactKind::Specification,
                content: render::render_specification_note(title, spec, table, &frontmatter),
            });
            let by_task: HashMap<&Handle, Vec<&str>> =
                spec.requirements.iter().fold(HashMap::new(), |mut map, r| {
                    map.entry(&r.task_ref)
                        .or_default()
                        .push(r.requirement.as_str());
                    map
                });
            contributions.push(UnitContribution {
                unit,
                evidence: host_evidence(table, &key, |task| {
                    by_task.get(task).map(|r| r.join(" ")).unwrap_or_default()
                }),
                artifacts: Vec::new(),
            });
            spec.objective.clone()
        }
        Stage::Contract => {
            let [(unit, _, CompletedPayloadV1::Contract(contract))] = payloads.as_slice() else {
                return Err(single("The contract"));
            };
            let key =
                Handle::new("contract_document").expect("built-in recipe key is a valid handle");
            let planned = planned_unit_rows(inputs, contract);
            let preserved = inputs
                .prior
                .get(&Stage::Specification)
                .and_then(specification_of)
                .is_some_and(|spec| contract.groups.is_empty() && contract_is_preserved(spec, table))
                .then_some("Preserved: the approved specification declares no new or changed interface and every cited source is delivered, so the host rendered this document without a model call. Approving it does not assert that the code already satisfies the preserved interfaces.");
            host_artifacts.push(HostArtifact {
                artifact_key: key.clone(),
                relative_path: None,
                kind: ArtifactKind::Contract,
                content: render::render_contract_document(
                    title, contract, table, preserved, &planned,
                ),
            });
            let by_task: HashMap<&Handle, Vec<String>> =
                contract
                    .groups
                    .iter()
                    .fold(HashMap::new(), |mut map, group| {
                        for criterion in &group.criterion_refs {
                            if let Some(binding) = table.criterion(criterion) {
                                map.entry(&binding.task)
                                    .or_default()
                                    .push(format!("`{}`", group.interface_key));
                            }
                        }
                        map
                    });
            contributions.push(UnitContribution {
                unit,
                evidence: host_evidence(table, &key, |task| {
                    by_task
                        .get(task)
                        .map(|groups| format!("Interfaces: {}.", groups.join(", ")))
                        .unwrap_or_else(|| "No interface change required.".into())
                }),
                artifacts: Vec::new(),
            });
            format!(
                "Contract defined {} interface group(s).",
                contract.groups.len()
            )
        }
        Stage::Code => {
            let mut files = 0usize;
            for (unit, child, payload) in &payloads {
                let CompletedPayloadV1::Edits(edits) = payload else {
                    return Err(single("A code result"));
                };
                let artifacts = reconstruct_artifacts(plan, preparation, child)?;
                files += artifacts.len();
                contributions.push(UnitContribution {
                    unit,
                    evidence: edits.evidence.clone(),
                    artifacts,
                });
            }
            format!(
                "Code produced {files} file(s) across {} unit(s).",
                payloads.len()
            )
        }
        Stage::Test => {
            let mut coverage = Vec::new();
            let mut cited = Vec::new();
            let mut files = 0usize;
            for (unit, child, payload) in &payloads {
                let CompletedPayloadV1::Tests(tests) = payload else {
                    return Err(single("A test result"));
                };
                let artifacts = reconstruct_artifacts(plan, preparation, child)?;
                files += artifacts.len();
                coverage.extend(tests.coverage.iter().cloned());
                cited.extend(
                    tests
                        .coverage
                        .iter()
                        .flat_map(|c| c.test_input_refs.iter().cloned())
                        .chain(unit.owned_destination_refs.iter().cloned()),
                );
                // Evidence that names no authored artifact (approved
                // not-applicable coverage) is carried by the host report so
                // the task still points at a durable artifact.
                let report_key =
                    Handle::new("test_report").expect("built-in recipe key is a valid handle");
                let evidence = tests
                    .evidence
                    .iter()
                    .cloned()
                    .map(|mut item| {
                        if item.artifact_keys.is_empty() {
                            item.artifact_keys.push(report_key.clone());
                        }
                        item
                    })
                    .collect();
                contributions.push(UnitContribution {
                    unit,
                    evidence,
                    artifacts,
                });
            }
            let cited = cited_file_refs(table, cited);
            let frontmatter = Frontmatter::from_references(table, &cited);
            host_artifacts.push(HostArtifact {
                artifact_key: Handle::new("test_report")
                    .expect("built-in recipe key is a valid handle"),
                relative_path: Some(vault_note_path("tests", title)?),
                kind: ArtifactKind::ValidationReport,
                content: render::render_test_report(title, &coverage, checks, table, &frontmatter),
            });
            format!(
                "Test authored {files} file(s) and declared coverage for {} criteria across {} unit(s).",
                coverage.len(),
                payloads.len()
            )
        }
        Stage::Integrator => {
            let mut assessments = Vec::new();
            let mut blockers = Vec::new();
            let mut per_unit = Vec::new();
            for (unit, child, payload) in &payloads {
                let CompletedPayloadV1::Integration(integration) = payload else {
                    return Err(single("An integration result"));
                };
                assessments.extend(integration.assessments.iter().cloned());
                blockers.extend(integration.blockers.iter().cloned());
                per_unit.push((*unit, reconstruct_artifacts(plan, preparation, child)?));
            }
            let key =
                Handle::new("integration_report").expect("built-in recipe key is a valid handle");
            host_artifacts.push(HostArtifact {
                artifact_key: key.clone(),
                relative_path: None,
                kind: ArtifactKind::IntegrationReport,
                content: render::render_integration_report(
                    title,
                    &assessments,
                    &blockers,
                    checks,
                    table,
                ),
            });
            findings.extend(blockers.iter().cloned());
            let by_task: HashMap<&Handle, Vec<String>> =
                assessments.iter().fold(HashMap::new(), |mut map, a| {
                    map.entry(&a.task_ref)
                        .or_default()
                        .push(format!("{}: {:?}", a.criterion_ref, a.verdict));
                    map
                });
            for (index, (unit, artifacts)) in per_unit.into_iter().enumerate() {
                contributions.push(UnitContribution {
                    unit,
                    evidence: if index == 0 {
                        host_evidence(table, &key, |task| {
                            by_task.get(task).map(|v| v.join("; ")).unwrap_or_default()
                        })
                    } else {
                        Vec::new()
                    },
                    artifacts,
                });
            }
            format!("Integration assessed {} criteria.", assessments.len())
        }
    };
    Ok(StageFinish {
        contributions,
        host_artifacts,
        payload_row,
        findings,
        summary,
    })
}

/// The approved proposals with the Contract's dependency refinements, paths
/// resolved through the Specification's own reference table, for the
/// Contract checkpoint document.
fn planned_unit_rows(
    inputs: &StageInputs,
    contract: &InterfaceContractV1,
) -> Vec<render::PlannedUnitRow> {
    let Some(record) = inputs.prior.get(&Stage::Specification) else {
        return Vec::new();
    };
    let Some(spec) = specification_of(record) else {
        return Vec::new();
    };
    spec.unit_proposals
        .iter()
        .map(|proposal| {
            let mut depends_on = proposal.depends_on.clone();
            if let Some(binding) = contract
                .unit_bindings
                .iter()
                .find(|binding| binding.unit_key == proposal.unit_key)
            {
                for dependency in &binding.depends_on {
                    if !depends_on.contains(dependency) {
                        depends_on.push(dependency.clone());
                    }
                }
            }
            render::PlannedUnitRow {
                key: proposal.unit_key.clone(),
                kind: match proposal.kind {
                    ProposalKind::Code => "code".into(),
                    ProposalKind::Test => "test".into(),
                },
                task_refs: proposal.task_refs.clone(),
                criterion_refs: proposal.criterion_refs.clone(),
                destinations: proposal
                    .destination_refs
                    .iter()
                    .map(|handle| {
                        record
                            .path_of(handle)
                            .map(|path| path.as_str().to_owned())
                            .unwrap_or_else(|| handle.as_str().to_owned())
                    })
                    .collect(),
                depends_on,
                objective: proposal.objective.clone(),
            }
        })
        .collect()
}

/// Check handles the approved specification made mandatory for some
/// criterion (`requiredCheckRefs`), promoted to `required` at validation.
pub fn promoted_check_refs(inputs: &StageInputs) -> HashSet<Handle> {
    inputs
        .prior
        .get(&Stage::Specification)
        .and_then(specification_of)
        .map(|spec| {
            spec.requirements
                .iter()
                .flat_map(|r| r.required_check_refs.iter().cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the approved specification settles the Contract without a model:
/// every interface intent is preserve/none and every source it cites is
/// delivered (recipes §Contract zero-call path).
pub fn contract_is_preserved(spec: &SpecificationContractV1, table: &ReferenceTable) -> bool {
    !spec.interface_intent.is_empty()
        && spec.interface_intent.iter().all(|intent| {
            matches!(
                intent.intent,
                InterfaceIntent::Preserve | InterfaceIntent::None
            ) && intent.source_refs.iter().all(|handle| {
                table
                    .entry(handle)
                    .is_some_and(ReferenceEntryV1::is_readable_input)
            })
        })
}

/// The response the host synthesizes for a unit the approved specification
/// already settles, or `None` when a model must answer.
pub fn zero_call_response(
    stage: Stage,
    inputs: &StageInputs,
    preparation: &StagePreparation,
    unit: &WorkUnitSpecV1,
) -> Option<UnitModelResponseV1> {
    let spec = inputs
        .prior
        .get(&Stage::Specification)
        .and_then(specification_of)?;
    match stage {
        Stage::Contract if contract_is_preserved(spec, &preparation.table) => {
            Some(UnitModelResponseV1::Completed {
                executor_kind: ExecutorKind::ContractAuthor,
                payload: CompletedPayloadV1::Contract(InterfaceContractV1 {
                    groups: Vec::new(),
                    unit_bindings: spec
                        .unit_proposals
                        .iter()
                        .map(|proposal| super::UnitInterfaceBindingV1 {
                            unit_key: proposal.unit_key.clone(),
                            interface_keys: Vec::new(),
                            depends_on: proposal.depends_on.clone(),
                        })
                        .collect(),
                    used_refs: Vec::new(),
                }),
            })
        }
        Stage::Test => {
            // Every criterion of the unit approved not_applicable: nothing to author.
            let reasons: HashMap<&Handle, &str> = spec
                .requirements
                .iter()
                .filter(|r| r.verification_mode == VerificationMode::NotApplicable)
                .filter_map(|r| {
                    r.not_applicable_reason
                        .as_deref()
                        .map(|reason| (&r.criterion_ref, reason))
                })
                .collect();
            if unit.criterion_refs.is_empty()
                || !unit.criterion_refs.iter().all(|c| reasons.contains_key(c))
            {
                return None;
            }
            let table = &preparation.table;
            let coverage = unit
                .criterion_refs
                .iter()
                .map(|criterion| super::TestCoverageV1 {
                    task_ref: table
                        .criterion(criterion)
                        .map(|binding| binding.task.clone())
                        .unwrap_or_else(|| unit.task_refs[0].clone()),
                    criterion_ref: criterion.clone(),
                    coverage: super::CoverageKind::NotApplicable,
                    test_artifact_keys: Vec::new(),
                    test_input_refs: Vec::new(),
                    check_refs: Vec::new(),
                    explanation: format!("Approved as not applicable: {}", reasons[criterion]),
                })
                .collect::<Vec<_>>();
            let evidence = unit
                .task_refs
                .iter()
                .map(|task| EvidenceItemV1 {
                    task_ref: task.clone(),
                    criterion_refs: unit
                        .criterion_refs
                        .iter()
                        .filter(|c| table.criterion(c).is_some_and(|b| &b.task == task))
                        .cloned()
                        .collect(),
                    artifact_keys: Vec::new(),
                    input_refs: Vec::new(),
                    explanation: "Every criterion of this task was approved as not applicable to testing; no test was authored.".into(),
                })
                .filter(|item| !item.criterion_refs.is_empty())
                .collect::<Vec<_>>();
            Some(UnitModelResponseV1::Completed {
                executor_kind: ExecutorKind::TestAuthor,
                payload: CompletedPayloadV1::Tests(TestPayloadV1 {
                    edits: Vec::new(),
                    coverage,
                    evidence,
                    used_refs: Vec::new(),
                }),
            })
        }
        _ => None,
    }
}

/// Deterministic Integrator pre-checks on the accepted payloads: every
/// criterion in scope must carry Code evidence and a Test coverage entry.
pub fn criterion_representation_findings(inputs: &StageInputs) -> Vec<String> {
    let Some(manifest) = inputs.manifest.as_ref() else {
        return Vec::new();
    };
    let code_covered: HashSet<Handle> = inputs
        .prior
        .get(&Stage::Code)
        .map(|record| {
            record
                .responses()
                .filter_map(|response| match response {
                    UnitModelResponseV1::Completed {
                        payload: CompletedPayloadV1::Edits(edits),
                        ..
                    } => Some(
                        edits
                            .evidence
                            .iter()
                            .flat_map(|e| e.criterion_refs.iter().cloned()),
                    ),
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .unwrap_or_default();
    let test_covered: HashSet<Handle> = inputs
        .prior
        .get(&Stage::Test)
        .map(|record| {
            record
                .responses()
                .filter_map(|response| match response {
                    UnitModelResponseV1::Completed {
                        payload: CompletedPayloadV1::Tests(tests),
                        ..
                    } => Some(tests.coverage.iter().map(|c| c.criterion_ref.clone())),
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .unwrap_or_default();
    let mut findings = Vec::new();
    for (task_index, task) in manifest.tasks.iter().enumerate() {
        for (criterion_index, criterion) in task.draft.criteria.iter().enumerate() {
            let handle = Handle::new(format!("crit_{}_{}", task_index + 1, criterion_index + 1))
                .expect("generated handle");
            if !code_covered.contains(&handle) {
                findings.push(format!(
                    "Criterion `{handle}` ({}) has no Code evidence in the accepted payload.",
                    criterion.id
                ));
            }
            if !test_covered.contains(&handle) {
                findings.push(format!(
                    "Criterion `{handle}` ({}) has no Test coverage entry in the accepted payload.",
                    criterion.id
                ));
            }
        }
    }
    findings
}

/// Extracts the typed payload records from accepted legacy artifacts.
pub fn prior_payloads(
    rows: &[(String, String)],
) -> Result<BTreeMap<Stage, StagePayloadRecord>, CompositionError> {
    let mut prior = BTreeMap::new();
    for (kind, content) in rows {
        if kind != STAGE_PAYLOAD_KIND {
            continue;
        }
        let record: StagePayloadRecord = serde_json::from_str(content).map_err(|error| {
            recipe_error(
                ErrorCode::HostContractError,
                format!("A persisted stage payload is unreadable: {error}"),
            )
        })?;
        prior.insert(record.stage, record);
    }
    Ok(prior)
}

/// Explicit workspace-relative paths named by a task's title or description.
pub fn explicit_paths(text: &str) -> Vec<RelPath> {
    let mut paths = BTreeSet::new();
    for token in text.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')
    }) {
        let token = token.trim_end_matches(['.', ':']);
        if (token.contains('/') || is_root_file_name(token))
            && token.contains('.')
            && !token.contains("://")
            && !token.starts_with('.')
            && let Ok(path) = RelPath::new(token)
        {
            paths.insert(path);
        }
    }
    paths.into_iter().collect()
}

/// A workspace-root file named without a directory (`index.html`,
/// `package.json`, `README.md`). Requiring a `/` left every root file
/// unanchored, so each greenfield `ToDoListWebView` pulsation spent a
/// Specification call and a clarification round asking for `index.html`
/// (2026-09-22). Only known source/config extensions count, and library
/// names written like files in prose (`Node.js`, `Vue.js`) are ignored.
fn is_root_file_name(token: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        "html", "htm", "css", "scss", "js", "mjs", "cjs", "jsx", "ts", "tsx", "json", "md", "toml",
        "yaml", "yml", "py", "rs", "go", "java", "cs", "rb", "php", "vue", "svelte", "sql", "sh",
        "ps1", "xml", "txt",
    ];
    const LIBRARY_NAMES: &[&str] = &[
        "node", "vue", "next", "nuxt", "react", "express", "three", "chart", "d3", "ember",
        "backbone", "socket", "moment", "angular", "alpine", "solid", "deno", "bun",
    ];
    let Some((stem, extension)) = token.rsplit_once('.') else {
        return false;
    };
    !token.contains('/')
        && !stem.is_empty()
        && stem
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && EXTENSIONS.contains(&extension)
        && !LIBRARY_NAMES.contains(&stem.to_ascii_lowercase().as_str())
}

/// Parses every `FILES_NEW:`/`FILES_MOD:`/`FILES:` tag line a developer (or,
/// from M5 onward, the Companion's synthesis prompt) writes anywhere in a
/// clarification answer (ADR-022 clarification loop, P2/A2.2) -- prose lines
/// before, between or after a tag line are simply skipped, never scanned for
/// a false-positive path mention, since a match still requires the trimmed
/// line itself to start with one of the three prefixes. This is deliberately
/// position-independent: `SYNTHESIS_PROMPT` (`hvc-ui/src/resolution_ui.rs`)
/// asks the Companion for a short prose summary *before* the tag lines, and a
/// developer typing a manual answer writes the same way, so a leading-block-
/// only scan would silently discard every tag it never reaches. Values are
/// comma-separated; a value that does not parse as a `RelPath` is dropped
/// rather than failing the whole answer, since a malformed tag should mean
/// "no extra anchor", never "reject the developer's answer". `FILES:` (no
/// suffix) is returned unresolved -- the host decides new-vs-existing by
/// checking the workspace, which this pure function has no access to (see
/// `submit_specification_answers`).
pub fn parse_clarification_tags(text: &str) -> (Vec<RelPath>, Vec<RelPath>, Vec<RelPath>) {
    let mut planned = Vec::new();
    let mut modified = Vec::new();
    let mut unresolved = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let (target, rest) = if let Some(rest) = line.strip_prefix("FILES_NEW:") {
            (&mut planned, rest)
        } else if let Some(rest) = line.strip_prefix("FILES_MOD:") {
            (&mut modified, rest)
        } else if let Some(rest) = line.strip_prefix("FILES:") {
            (&mut unresolved, rest)
        } else {
            continue;
        };
        for value in rest.split(',') {
            let value = value.trim();
            if !value.is_empty()
                && let Ok(path) = RelPath::new(value)
            {
                target.push(path);
            }
        }
    }
    (planned, modified, unresolved)
}

pub fn contract_of(record: &StagePayloadRecord) -> Option<&InterfaceContractV1> {
    match record.primary()? {
        UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Contract(contract),
            ..
        } => Some(contract),
        _ => None,
    }
}

pub fn specification_of(record: &StagePayloadRecord) -> Option<&SpecificationContractV1> {
    match record.primary()? {
        UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Specification(spec),
            ..
        } => Some(spec),
        _ => None,
    }
}

pub fn intake_of(record: &StagePayloadRecord) -> Option<&IntakePayloadV1> {
    match record.primary()? {
        UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Intake(intake),
            ..
        } => Some(intake),
        _ => None,
    }
}

pub fn tests_of(record: &StagePayloadRecord) -> Option<&TestPayloadV1> {
    match record.primary()? {
        UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Tests(tests),
            ..
        } => Some(tests),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_forge::{
        AcceptanceCriterion, Feature, FeatureBrief, FeatureEvaluation, FeatureState, TaskDraft,
        TaskKind, WorkTask,
    };

    fn manifest() -> BatchManifest {
        let task = |id: &str, title: &str, description: &str, criteria: &[&str]| WorkTask {
            id: id.into(),
            project_id: "project".into(),
            revision: 1,
            draft: TaskDraft {
                title: title.into(),
                kind: TaskKind::Feature,
                description: description.into(),
                criteria: criteria
                    .iter()
                    .enumerate()
                    .map(|(i, c)| AcceptanceCriterion {
                        id: format!("AC-{}", i + 1),
                        description: c.to_string(),
                    })
                    .collect(),
                dependencies: vec![],
                feature_id: None,
            },
            feature_revision: None,
            state: "queued".into(),
            active_pulsation_id: None,
            completed_pulsation_id: None,
        };
        BatchManifest {
            schema_version: 1,
            title: "Login".into(),
            path_confirmation_source: crate::feature_forge::PathConfirmationSource::default(),
            tasks: vec![
                task(
                    "t-1",
                    "Reject expired tokens",
                    "Change src/auth.rs so expired tokens fail.",
                    &["Expired tokens are rejected.", "Valid tokens pass."],
                ),
                task(
                    "t-2",
                    "Document the flow",
                    "Update docs/auth.md.",
                    &["The document explains expiry."],
                ),
            ],
            features: vec![Feature {
                id: "f-1".into(),
                project_id: "project".into(),
                title: "Auth".into(),
                original_idea: "idea".into(),
                revision: 1,
                approved_revision: Some(1),
                state: FeatureState::Executing,
                brief: FeatureBrief {
                    objective: "Secure login.".into(),
                    ..Default::default()
                },
                evaluation: FeatureEvaluation::default(),
                structural_level: 0,
                wave: None,
                position: 0,
                archive_reason: None,
            }],
            feature_dependencies: vec![],
        }
    }

    fn inputs(stage_sources: Vec<SourceFile>, missing: Vec<&str>) -> StageInputs {
        StageInputs {
            title: "Login".into(),
            manifest: Some(manifest()),
            prior: BTreeMap::new(),
            sources: stage_sources,
            missing_paths: missing.iter().map(|m| RelPath::new(*m).unwrap()).collect(),
            checks: vec![],
            revision_feedback: None,
            clarification_answers: vec![],
            qualified_recipes: vec!["code".into(), "test".into()],
            scope_amendments: Vec::new(),
        }
    }

    #[test]
    fn explicit_task_paths_and_roles_are_derived_deterministically() {
        assert_eq!(
            explicit_paths(
                "Change `src/auth.rs` and docs/auth.md, then tests/auth_test.rs; see https://x.y/z."
            ),
            vec![
                RelPath::new("docs/auth.md").unwrap(),
                RelPath::new("src/auth.rs").unwrap(),
                RelPath::new("tests/auth_test.rs").unwrap()
            ]
        );
        // Root files are anchored too; prose that only looks like a file is not.
        assert_eq!(
            explicit_paths(
                "Build the views (index.html, src/js/app.js) with package.json and README.md; built on Node.js and Vue.js, e.g. v1.2 in 3.5 seconds."
            ),
            vec![
                RelPath::new("README.md").unwrap(),
                RelPath::new("index.html").unwrap(),
                RelPath::new("package.json").unwrap(),
                RelPath::new("src/js/app.js").unwrap(),
            ]
        );
        assert_eq!(
            role_for_path(&RelPath::new("tests/auth_test.rs").unwrap()),
            SourceRole::Test
        );
        assert_eq!(
            role_for_path(&RelPath::new("src/auth.rs").unwrap()),
            SourceRole::Source
        );
        assert_eq!(
            role_for_path(&RelPath::new("docs/auth.md").unwrap()),
            SourceRole::Document
        );
        assert_eq!(
            role_for_path(&RelPath::new("Cargo.toml").unwrap()),
            SourceRole::Configuration
        );
        let src = SourceFile {
            path: RelPath::new("src/auth.rs").unwrap(),
            role: SourceRole::Source,
            content: "fn a() {}\n".into(),
            origin: SourceOrigin::Brief,
        };
        assert_eq!(
            default_test_destination("User Login", &[src])
                .unwrap()
                .as_str(),
            "tests/user_login_test.rs"
        );
        assert!(default_test_destination("x", &[]).is_none());
    }

    // ADR-022 clarification loop (P2/A2.2): `parse_clarification_tags` reads
    // `FILES_*:` tag lines wherever they appear in a clarification answer,
    // leaving the prose (including the raw tag lines) untouched for
    // `batch_guidance`. Prose mentioning a path in passing is never matched:
    // a line only counts as a tag when the trimmed line itself starts with
    // one of the three prefixes, regardless of position.
    #[test]
    fn clarification_tags_are_parsed_wherever_they_appear_not_mentions_in_prose() {
        let (planned, modified, unresolved) = parse_clarification_tags(
            "FILES_NEW: src/foo.rs, src/bar.rs\nFILES_MOD: src/existing.rs\n\nProsa livre que menciona src/never.rs mas não é uma tag.",
        );
        assert_eq!(
            planned,
            vec![
                RelPath::new("src/foo.rs").unwrap(),
                RelPath::new("src/bar.rs").unwrap()
            ]
        );
        assert_eq!(modified, vec![RelPath::new("src/existing.rs").unwrap()]);
        assert!(unresolved.is_empty());
    }

    // Reproduces the exact shape `SYNTHESIS_PROMPT` (`hvc-ui/src/resolution_ui.rs`)
    // asks the Companion for -- and that a developer typing a manual answer
    // naturally writes too: a short prose summary first, tag lines after. A
    // leading-block-only scan silently discarded these (the bug this session
    // found via two real clarification answers in the app's own database
    // whose `plannedPaths`/`modifiedPaths` came back empty despite the tags
    // being present in `textAnswer`).
    #[test]
    fn a_prose_summary_before_the_tag_lines_does_not_suppress_them() {
        let (planned, modified, unresolved) = parse_clarification_tags(
            "Create a planned-destination handle for the application shell, and provide existing-file handles for both domain models.\n\nFILES_NEW: index.html\nFILES_MOD: src/js/models/task.js, src/js/models/calendarSettings.js",
        );
        assert_eq!(planned, vec![RelPath::new("index.html").unwrap()]);
        assert_eq!(
            modified,
            vec![
                RelPath::new("src/js/models/task.js").unwrap(),
                RelPath::new("src/js/models/calendarSettings.js").unwrap()
            ]
        );
        assert!(unresolved.is_empty());
    }

    #[test]
    fn a_bare_files_tag_is_returned_unresolved_for_the_host_to_check_the_disk() {
        let (planned, modified, unresolved) =
            parse_clarification_tags("FILES: src/auth.rs, src/new_module.rs");
        assert!(planned.is_empty());
        assert!(modified.is_empty());
        assert_eq!(
            unresolved,
            vec![
                RelPath::new("src/auth.rs").unwrap(),
                RelPath::new("src/new_module.rs").unwrap()
            ]
        );
    }

    #[test]
    fn text_with_no_tag_lines_parses_to_nothing_same_as_before_this_milestone() {
        let (planned, modified, unresolved) =
            parse_clarification_tags("JWT assinado, sem nenhuma tag de arquivo.");
        assert!(planned.is_empty());
        assert!(modified.is_empty());
        assert!(unresolved.is_empty());
    }

    #[test]
    fn an_invalid_path_value_is_dropped_not_fatal() {
        // A backslash is a forbidden RelPath character; the malformed value
        // is silently dropped, the well-formed one beside it still lands.
        let (planned, _, _) = parse_clarification_tags("FILES_NEW: src\\bad.rs, src/good.rs");
        assert_eq!(planned, vec![RelPath::new("src/good.rs").unwrap()]);
    }

    #[test]
    fn intake_and_specification_prepare_one_unit_over_the_whole_batch() {
        let src = SourceFile {
            path: RelPath::new("src/auth.rs").unwrap(),
            role: SourceRole::Source,
            content: "fn a() {}\n".into(),
            origin: SourceOrigin::Brief,
        };
        let intake = prepare(Stage::Intake, &inputs(vec![src.clone()], vec![])).unwrap();
        assert_eq!(intake.drafts.len(), 1);
        let draft = &intake.drafts[0];
        assert_eq!(draft.executor_kind, ExecutorKind::IntakeClassifier);
        assert_eq!(draft.task_refs.len(), 2);
        assert_eq!(draft.criterion_refs.len(), 3);
        assert!(
            intake.delivered.is_empty(),
            "Intake gets handles, not file bodies"
        );
        assert!(
            intake
                .extra_instructions
                .as_deref()
                .unwrap()
                .contains("Secure login.")
        );
        let spec = prepare(
            Stage::Specification,
            &inputs(vec![src], vec!["docs/auth.md"]),
        )
        .unwrap();
        assert_eq!(
            spec.drafts[0].executor_kind,
            ExecutorKind::SpecificationAuthor
        );
        assert!(spec.delivered.contains_key(&Handle::new("src_1").unwrap()));
        assert!(spec.table.entry(&Handle::new("plan_1").unwrap()).is_some());
        assert!(
            spec.extra_instructions
                .as_deref()
                .unwrap()
                .contains("`plan_1`")
        );
        assert!(spec.drafts[0].owned_destination_refs.is_empty());
        // Greenfield: no sources at all still yields a valid preparation with an unavailable handle.
        let greenfield = prepare(Stage::Specification, &inputs(vec![], vec![])).unwrap();
        assert!(
            greenfield.drafts[0]
                .input_refs
                .iter()
                .any(|h| h.as_str().starts_with("unav_"))
        );
    }

    #[test]
    fn code_and_test_own_the_right_files_and_anchor_delivered_sources() {
        let src = SourceFile {
            path: RelPath::new("src/auth.rs").unwrap(),
            role: SourceRole::Source,
            content: "fn a() {}\nfn b() {}\n".into(),
            origin: SourceOrigin::Brief,
        };
        let doc = SourceFile {
            path: RelPath::new("docs/auth.md").unwrap(),
            role: SourceRole::Document,
            content: "# Auth\n".into(),
            origin: SourceOrigin::Brief,
        };
        let mut stage_inputs = inputs(
            vec![src.clone(), doc.clone()],
            vec!["src/token.rs", "tests/token_test.rs"],
        );
        // Code needs the approved specification record.
        let spec_record = StagePayloadRecord {
            schema_version: SchemaV1,
            stage: Stage::Specification,
            plan_sha256: super::super::Sha256Hex::of(b"p"),
            references: vec![],
            units: vec![UnitPayloadRecord {
                logical_unit_key: Handle::new("specify_batch").unwrap(),
                response: UnitModelResponseV1::Completed {
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                        objective: "o".into(),
                        exclusions: vec![],
                        decisions: vec!["Keep signatures.".into()],
                        requirements: vec![super::super::RequirementV1 {
                            task_ref: Handle::new("task_2").unwrap(),
                            criterion_ref: Handle::new("crit_2_1").unwrap(),
                            requirement: "Explain expiry.".into(),
                            verification_intent: "Read the document.".into(),
                            verification_mode: VerificationMode::NotApplicable,
                            required_check_refs: vec![],
                            not_applicable_reason: Some("Documentation only.".into()),
                        }],
                        existing_file_refs: vec![],
                        planned_file_refs: vec![],
                        interface_intent: vec![super::super::InterfaceIntentV1 {
                            intent: super::super::InterfaceIntent::Preserve,
                            source_refs: vec![],
                            criterion_refs: vec![Handle::new("crit_1_1").unwrap()],
                            description: "d".into(),
                        }],
                        unit_proposals: vec![],
                    }),
                },
            }],
        };
        stage_inputs.prior.insert(Stage::Specification, spec_record);
        let code = prepare(Stage::Code, &stage_inputs).unwrap();
        let draft = &code.drafts[0];
        assert_eq!(draft.executor_kind, ExecutorKind::CodeImplementer);
        // Owns the source, the document and the planned source; not the planned test.
        assert_eq!(
            draft.owned_destination_refs,
            vec![
                Handle::new("src_1").unwrap(),
                Handle::new("src_2").unwrap(),
                Handle::new("plan_1").unwrap()
            ]
        );
        // One anchor per Rust symbol (`fn a`, `fn b`) and one for the document.
        assert_eq!(code.table.anchors().len(), 3);
        let anchors = code.table.anchors();
        assert_eq!(anchors[0].source_ref, Handle::new("src_1").unwrap());
        assert_eq!((anchors[0].start_byte, anchors[0].end_byte), (0, 10));
        assert_eq!((anchors[1].start_byte, anchors[1].end_byte), (10, 20));
        assert_eq!(anchors[2].source_ref, Handle::new("src_2").unwrap());
        assert_eq!(
            code.approved.not_applicable_criteria,
            vec![Handle::new("crit_2_1").unwrap()]
        );
        assert_eq!(code.decisions, vec!["Keep signatures."]);
        assert_eq!(code.requirements.len(), 1);
        let test = prepare(Stage::Test, &stage_inputs).unwrap();
        assert_eq!(test.drafts[0].executor_kind, ExecutorKind::TestAuthor);
        assert_eq!(
            test.drafts[0].owned_destination_refs,
            vec![Handle::new("plan_1").unwrap()]
        );
        assert_eq!(
            test.table
                .entry(&Handle::new("plan_1").unwrap())
                .unwrap()
                .relative_path()
                .unwrap()
                .as_str(),
            "tests/token_test.rs"
        );
        // Without a planned test, a deterministic default destination is admitted.
        let mut no_test = stage_inputs.clone();
        no_test.missing_paths.clear();
        let test = prepare(Stage::Test, &no_test).unwrap();
        assert_eq!(
            test.table
                .entry(&test.drafts[0].owned_destination_refs[0])
                .unwrap()
                .relative_path()
                .unwrap()
                .as_str(),
            "tests/login_test.rs"
        );
        // Integrator owns documents only.
        let integrator = prepare(Stage::Integrator, &stage_inputs).unwrap();
        assert_eq!(
            integrator.drafts[0].owned_destination_refs,
            vec![Handle::new("src_2").unwrap()]
        );
        // ADR-032: stage documents and the review policy never reach the
        // reviewer, and a document a stage published is a candidate it
        // reads but cannot edit. No stage owns the review policy.
        let mut reviewed = stage_inputs.clone();
        for (path, origin) in [
            (
                "knowledge/obsidian/tests/login.md",
                SourceOrigin::StageArtifact {
                    stage: Stage::Test,
                    artifact_kind: "validation_report".into(),
                },
            ),
            (
                "knowledge/obsidian/features/login.md",
                SourceOrigin::StageArtifact {
                    stage: Stage::Specification,
                    artifact_kind: "specification".into(),
                },
            ),
            (
                "docs/changelog.md",
                SourceOrigin::StageArtifact {
                    stage: Stage::Code,
                    artifact_kind: "documentation".into(),
                },
            ),
            ("REVIEW.md", SourceOrigin::Brief),
        ] {
            reviewed.sources.push(SourceFile {
                path: RelPath::new(path).unwrap(),
                role: SourceRole::Document,
                content: "# Note\n".into(),
                origin,
            });
        }
        let paths = |table: &ReferenceTable, handles: &[Handle]| {
            handles
                .iter()
                .filter_map(|handle| table.entry(handle)?.relative_path())
                .map(|path| path.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        let integrator = prepare(Stage::Integrator, &reviewed).unwrap();
        let draft = &integrator.drafts[0];
        let delivered = paths(&integrator.table, &draft.input_refs);
        assert!(delivered.contains(&"docs/changelog.md".to_owned()));
        for hidden in [
            "knowledge/obsidian/tests/login.md",
            "knowledge/obsidian/features/login.md",
            "REVIEW.md",
        ] {
            assert!(!delivered.contains(&hidden.to_owned()), "{hidden}");
        }
        assert_eq!(
            paths(&integrator.table, &draft.owned_destination_refs),
            ["docs/auth.md"]
        );
        let code = prepare(Stage::Code, &reviewed).unwrap();
        let owned = paths(&code.table, &code.drafts[0].owned_destination_refs);
        assert!(owned.contains(&"docs/auth.md".to_owned()), "{owned:?}");
        assert!(!owned.contains(&"REVIEW.md".to_owned()), "{owned:?}");
        // Payload records round-trip through the legacy artifact rows.
        let rows = vec![
            (
                STAGE_PAYLOAD_KIND.to_string(),
                serde_json::to_string(&stage_inputs.prior[&Stage::Specification]).unwrap(),
            ),
            ("code".to_string(), "x".into()),
        ];
        let prior = prior_payloads(&rows).unwrap();
        assert!(specification_of(&prior[&Stage::Specification]).is_some());
        assert!(contract_of(&prior[&Stage::Specification]).is_none());
        assert_eq!(
            clarification_questions(&[super::super::QuestionV1 {
                key: Handle::new("k").unwrap(),
                task_refs: vec![Handle::new("task_1").unwrap()],
                question: "Which?".into(),
                rationale: "Why.".into()
            }])[0]
                .id,
            "q_1"
        );
    }

    /// `ToDoListWebView`, 2026-09-22: both pulsations failed Code with
    /// `scope_insufficient` because the first unit (owning only
    /// `localStore.js`) read the batch-wide title, guidance and a clarification
    /// naming the other four files as missing destinations. The plan keeps
    /// exclusive ownership; every unit's prompt now names its siblings' files
    /// as context, serial or parallel.
    #[test]
    fn a_split_code_stage_tells_each_unit_its_siblings_files_are_context() {
        use super::super::plan::{PlanInputs, freeze_plan};
        use super::super::policy::policy_for;
        use super::super::projection::render_unit_prompt;
        use super::super::schema::SchemaFlavor;
        use super::super::{ParentFenceV1, Uuid};

        let files = [
            "index.html",
            "src/js/app.js",
            "src/js/views/calendarView.js",
            "src/js/views/dayDetailView.js",
            "src/js/storage/localStore.js",
        ];
        let mut batch = manifest();
        batch.tasks.truncate(1);
        let task = &mut batch.tasks[0];
        task.draft.title = format!(
            "Build local task management and calendar views ({})",
            files.join(", ")
        );
        task.draft.description = format!("Files: {}.", files.join(", "));
        task.draft.criteria.truncate(1);
        task.draft.criteria.push(AcceptanceCriterion {
            id: "AC-2".into(),
            description: "Tasks survive browser reloads.".into(),
        });
        let spec_handles = ["plan_1", "plan_2", "plan_3", "plan_4", "plan_5"];
        let h = |text: &str| Handle::new(text).unwrap();
        let proposal =
            |key: &str, criterion: &str, owned: &[&str], depends: &[&str]| UnitProposalV1 {
                unit_key: h(key),
                kind: ProposalKind::Code,
                task_refs: vec![h("task_1")],
                criterion_refs: vec![h(criterion)],
                input_refs: vec![],
                destination_refs: owned.iter().map(|o| h(o)).collect(),
                depends_on: depends.iter().map(|d| h(d)).collect(),
                objective: format!("Implement {key}."),
            };
        let spec_record = StagePayloadRecord {
            schema_version: SchemaV1,
            stage: Stage::Specification,
            plan_sha256: Sha256Hex::of(b"p"),
            references: files
                .iter()
                .zip(spec_handles)
                .map(|(path, handle)| ReferenceEntryV1::PlannedDestination {
                    handle: h(handle),
                    relative_path: RelPath::new(*path).unwrap(),
                    source_role: SourceRole::Source,
                })
                .collect(),
            units: vec![UnitPayloadRecord {
                logical_unit_key: h("specify_batch"),
                response: UnitModelResponseV1::Completed {
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                        objective: "o".into(),
                        exclusions: vec![],
                        decisions: vec![],
                        requirements: vec![],
                        existing_file_refs: vec![],
                        planned_file_refs: spec_handles.iter().map(|p| h(p)).collect(),
                        interface_intent: vec![],
                        unit_proposals: vec![
                            proposal("local_store_code", "crit_1_2", &["plan_5"], &[]),
                            proposal(
                                "task_lifecycle_code",
                                "crit_1_1",
                                &["plan_1", "plan_2", "plan_3", "plan_4"],
                                &["local_store_code"],
                            ),
                        ],
                    }),
                },
            }],
        };
        let mut stage_inputs = inputs(vec![], files.to_vec());
        stage_inputs.manifest = Some(batch);
        stage_inputs.clarification_answers = vec![(
            "Please provide a planned-destination handle for index.html.".into(),
            "It is required by this batch.\nFILES_NEW: index.html".into(),
        )];
        stage_inputs.prior.insert(Stage::Specification, spec_record);
        let code = prepare(Stage::Code, &stage_inputs).unwrap();
        assert_eq!(code.drafts.len(), 2, "exclusive ownership keeps two units");

        for max_in_flight in [1, 2] {
            let mut snapshots = BTreeMap::new();
            snapshots.insert(ExecutorKind::CodeImplementer, Uuid::generate());
            let plan = freeze_plan(PlanInputs {
                stage_execution_id: Uuid::generate(),
                budget_account_id: Uuid::generate(),
                parent_fence: ParentFenceV1 {
                    project_id: Uuid::generate(),
                    pulsation_id: Uuid::generate(),
                    input_revision: 1,
                    job_id: Uuid::generate(),
                    generation: 1,
                    state_version: 1,
                },
                stage: Stage::Code,
                policy_sha256: Sha256Hex::of(b"policy"),
                batch_sha256: Sha256Hex::of(b"batch"),
                approved_contract_sha256: None,
                table: &code.table,
                units: code.drafts.clone(),
                stage_output_limit: 16_000,
                context_chars_limit: 16_000,
                max_in_flight_units: max_in_flight,
                profile_snapshots: snapshots,
            })
            .unwrap();
            let first = &plan.units[0];
            assert_eq!(first.logical_unit_key, h("local_store_code"));
            assert_eq!(first.owned_destination_refs.len(), 1);
            let prompt = render_unit_prompt(
                &plan,
                first,
                &code.table,
                policy_for(ExecutorKind::CodeImplementer),
                &prompt_inputs(&code, first),
                SchemaFlavor::Strict,
            );
            let user = &prompt.user;
            assert!(user.contains("This unit delivers only: `src/js/storage/localStore.js`"));
            assert!(user.contains(
                "- unit task_lifecycle_code: `index.html`, `src/js/app.js`, `src/js/views/calendarView.js`, `src/js/views/dayDetailView.js`"
            ));
            assert!(user.contains("do not fail with `scope_insufficient` because of it"));
            // The batch context the worker misread is still delivered, now
            // after the boundary and labelled as shared.
            assert!(user.contains("## Stage guidance (whole batch, shared by every unit)"));
            assert!(user.contains("FILES_NEW: index.html"));
            assert!(user.find("## Scope of this unit") < user.find("## Stage guidance"));
            // Ownership is unchanged: no sibling destination is citable here.
            let second = &plan.units[1];
            for handle in &second.owned_destination_refs {
                assert!(!user.contains(&format!("| `{handle}` |")));
            }
            let sibling_prompt = render_unit_prompt(
                &plan,
                second,
                &code.table,
                policy_for(ExecutorKind::CodeImplementer),
                &prompt_inputs(&code, second),
                SchemaFlavor::Strict,
            );
            assert!(
                sibling_prompt
                    .user
                    .contains("- unit local_store_code: `src/js/storage/localStore.js`")
            );
        }
    }

    /// "Teste Haiku Only 1" (2026-09-24): a revision of Code ran after the
    /// first run had published its files, so the planner sized them by their
    /// bytes and grouped the units differently. Files the approved
    /// Specification planned keep their planned estimate.
    #[test]
    fn publishing_the_planned_files_does_not_regroup_a_revised_code_stage() {
        let h = |text: &str| Handle::new(text).unwrap();
        let files = ["src/js/storage/localStore.js", "src/js/views/listView.js"];
        let proposal = |key: &str, criterion: &str, owned: &str| UnitProposalV1 {
            unit_key: h(key),
            kind: ProposalKind::Code,
            task_refs: vec![h("task_1")],
            criterion_refs: vec![h(criterion)],
            input_refs: vec![],
            destination_refs: vec![h(owned)],
            depends_on: vec![],
            objective: format!("Implement {key}."),
        };
        let spec_record = StagePayloadRecord {
            schema_version: SchemaV1,
            stage: Stage::Specification,
            plan_sha256: Sha256Hex::of(b"p"),
            references: files
                .iter()
                .zip(["plan_1", "plan_2"])
                .map(|(path, handle)| ReferenceEntryV1::PlannedDestination {
                    handle: h(handle),
                    relative_path: RelPath::new(*path).unwrap(),
                    source_role: SourceRole::Source,
                })
                .collect(),
            units: vec![UnitPayloadRecord {
                logical_unit_key: h("specify_batch"),
                response: UnitModelResponseV1::Completed {
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                        objective: "o".into(),
                        exclusions: vec![],
                        decisions: vec![],
                        requirements: vec![],
                        existing_file_refs: vec![],
                        planned_file_refs: vec![h("plan_1"), h("plan_2")],
                        interface_intent: vec![],
                        unit_proposals: vec![
                            proposal("store", "crit_1_1", "plan_1"),
                            proposal("view", "crit_1_2", "plan_2"),
                        ],
                    }),
                },
            }],
        };
        let units = |sources: Vec<SourceFile>, missing: Vec<&str>| {
            let mut batch = manifest();
            batch.tasks.truncate(1);
            let mut stage_inputs = inputs(sources, missing);
            stage_inputs.manifest = Some(batch);
            stage_inputs
                .prior
                .insert(Stage::Specification, spec_record.clone());
            prepare(Stage::Code, &stage_inputs)
                .unwrap()
                .drafts
                .into_iter()
                .map(|draft| (draft.logical_unit_key, draft.criterion_refs))
                .collect::<Vec<_>>()
        };
        let first_run = units(vec![], files.to_vec());
        // After the first run published both files (8 KB each, large enough
        // that sizing them by bytes would keep the units apart).
        let published = files
            .iter()
            .map(|path| SourceFile {
                path: RelPath::new(*path).unwrap(),
                role: SourceRole::Source,
                content: "x".repeat(8_000),
                origin: SourceOrigin::Brief,
            })
            .collect();
        let revision = units(published, vec![]);
        assert_eq!(first_run.len(), 1, "two small planned files coalesce");
        assert_eq!(revision, first_run);
    }

    /// Greenfield read-after-write: a view that only declares it reads the
    /// planned store module becomes a dependent unit that receives the store's
    /// validated candidate, instead of silently losing that input.
    #[test]
    fn a_planned_input_written_by_a_sibling_becomes_a_dependency_with_its_candidate() {
        let h = |text: &str| Handle::new(text).unwrap();
        let proposal =
            |key: &str, criterion: &str, inputs: &[&str], owned: &[&str]| UnitProposalV1 {
                unit_key: h(key),
                kind: ProposalKind::Code,
                task_refs: vec![h("task_1")],
                criterion_refs: vec![h(criterion)],
                input_refs: inputs.iter().map(|i| h(i)).collect(),
                destination_refs: owned.iter().map(|o| h(o)).collect(),
                depends_on: vec![],
                objective: format!("Implement {key}."),
            };
        let files = [
            "src/js/storage/localStore.js",
            "src/js/views/calendarView.js",
        ];
        let spec_record = StagePayloadRecord {
            schema_version: SchemaV1,
            stage: Stage::Specification,
            plan_sha256: Sha256Hex::of(b"p"),
            references: files
                .iter()
                .zip(["plan_1", "plan_2"])
                .map(|(path, handle)| ReferenceEntryV1::PlannedDestination {
                    handle: h(handle),
                    relative_path: RelPath::new(*path).unwrap(),
                    source_role: SourceRole::Source,
                })
                .collect(),
            units: vec![UnitPayloadRecord {
                logical_unit_key: h("specify_batch"),
                response: UnitModelResponseV1::Completed {
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                        objective: "o".into(),
                        exclusions: vec![],
                        decisions: vec![],
                        requirements: vec![],
                        existing_file_refs: vec![],
                        planned_file_refs: vec![h("plan_1"), h("plan_2")],
                        interface_intent: vec![],
                        unit_proposals: vec![
                            proposal("view", "crit_1_2", &["plan_1", "plan_1"], &["plan_2"]),
                            proposal("store", "crit_1_1", &[], &["plan_1"]),
                        ],
                    }),
                },
            }],
        };
        let mut batch = manifest();
        batch.tasks.truncate(1);
        let mut stage_inputs = inputs(vec![], files.to_vec());
        stage_inputs.manifest = Some(batch);
        stage_inputs.prior.insert(Stage::Specification, spec_record);
        let code = prepare(Stage::Code, &stage_inputs).unwrap();
        let view = code
            .drafts
            .iter()
            .find(|draft| draft.logical_unit_key == h("view"))
            .expect("the view stays its own unit");
        assert_eq!(view.depends_on, vec![h("store")]);
        let candidate = view
            .input_refs
            .iter()
            .find_map(|handle| match code.table.entry(handle) {
                Some(ReferenceEntryV1::CandidateSource { relative_path, .. }) => {
                    Some(relative_path.as_str())
                }
                _ => None,
            });
        assert_eq!(candidate, Some("src/js/storage/localStore.js"));
        let candidate_refs = view
            .input_refs
            .iter()
            .filter(|handle| {
                matches!(
                    code.table.entry(handle),
                    Some(ReferenceEntryV1::CandidateSource { relative_path, .. })
                        if relative_path.as_str() == "src/js/storage/localStore.js"
                )
            })
            .count();
        assert_eq!(
            candidate_refs, 1,
            "repeated reads share one candidate reference"
        );
    }

    /// A proposal that writes a file the Specification never planned is a
    /// real gap: it fails in preparation, before any reservation or CLI call.
    #[test]
    fn a_code_proposal_writing_an_unplanned_file_fails_before_dispatch() {
        let h = |text: &str| Handle::new(text).unwrap();
        let spec_record = StagePayloadRecord {
            schema_version: SchemaV1,
            stage: Stage::Specification,
            plan_sha256: Sha256Hex::of(b"p"),
            references: vec![ReferenceEntryV1::PlannedDestination {
                handle: h("plan_1"),
                relative_path: RelPath::new("src/js/app.js").unwrap(),
                source_role: SourceRole::Source,
            }],
            units: vec![UnitPayloadRecord {
                logical_unit_key: h("specify_batch"),
                response: UnitModelResponseV1::Completed {
                    executor_kind: ExecutorKind::SpecificationAuthor,
                    payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                        objective: "o".into(),
                        exclusions: vec![],
                        decisions: vec![],
                        requirements: vec![],
                        existing_file_refs: vec![],
                        planned_file_refs: vec![h("plan_1")],
                        interface_intent: vec![],
                        unit_proposals: vec![UnitProposalV1 {
                            unit_key: h("app_code"),
                            kind: ProposalKind::Code,
                            task_refs: vec![h("task_1")],
                            criterion_refs: vec![h("crit_1_1")],
                            input_refs: vec![],
                            // `plan_9` resolves to no path of the approved plan.
                            destination_refs: vec![h("plan_1"), h("plan_9")],
                            depends_on: vec![],
                            objective: "Implement the shell.".into(),
                        }],
                    }),
                },
            }],
        };
        let mut stage_inputs = inputs(vec![], vec!["src/js/app.js"]);
        stage_inputs.prior.insert(Stage::Specification, spec_record);
        let error = prepare(Stage::Code, &stage_inputs).unwrap_err();
        assert_eq!(error.code, ErrorCode::ScopeInsufficient);
        assert!(error.message.contains("neither delivered nor planned"));
    }
}
