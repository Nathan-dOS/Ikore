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
use super::validate::{ApprovedContext, ValidatedChild};
use super::{
    ArtifactKind, CheckSummaryV1, CompletedPayloadV1, CompositionError, ErrorCode, EvidenceItemV1,
    ExecutorKind, Handle, IntakePayloadV1, InterfaceContractV1, InterfaceGroupV1, InterfaceIntent,
    Invariant, MAX_REPLACE_FILE_BYTES, ProposalKind, ReferenceEntryV1, RelPath, Sha256Hex,
    SourceRole, SpecificationContractV1, Stage, StageExecutionPlanV1, TestPayloadV1,
    UnavailableReason, UnitModelResponseV1, UnitProposalV1, VerificationMode, WorkUnitSpecV1,
    scalar_len,
};
use crate::feature_forge::BatchManifest;
use crate::orchestration::{SchemaV1, SpecificationQuestion, StageArtifactDraft};

pub const STAGE_PAYLOAD_KIND: &str = "stage_payload_v1";

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
}

impl StageInputs {
    fn plans_units(&self, stage: Stage) -> bool {
        self.qualified_recipes
            .iter()
            .any(|recipe| recipe == stage.as_str())
    }
}

/// The frozen preparation of one stage before dispatch.
#[derive(Debug, Clone)]
pub struct StagePreparation {
    pub table: ReferenceTable,
    pub drafts: Vec<UnitDraft>,
    pub approved: ApprovedContext,
    pub delivered: BTreeMap<Handle, String>,
    pub requirements: Vec<(Handle, String)>,
    pub decisions: Vec<String>,
    pub interfaces: Vec<InterfaceGroupV1>,
    pub extra_instructions: Option<String>,
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

fn batch_guidance(manifest: &BatchManifest, inputs: &StageInputs) -> String {
    let mut text = String::new();
    for (position, task) in manifest.tasks.iter().enumerate() {
        text.push_str(&format!(
            "task_{}: {} ({:?}) - {}\n",
            position + 1,
            task.draft.title,
            task.draft.kind,
            task.draft.description.trim()
        ));
    }
    for feature in &manifest.features {
        let brief = &feature.brief;
        text.push_str(&format!(
            "\nFeature `{}`: objective: {} behavior: {} scope: {} exclusions: {}\n",
            feature.title,
            brief.objective.trim(),
            brief.behavior.trim(),
            brief.scope.trim(),
            brief.exclusions.trim()
        ));
        for decision in &brief.decisions {
            text.push_str(&format!("- decision: {decision}\n"));
        }
        for pending in &brief.pending_decisions {
            text.push_str(&format!("- pending decision: {pending}\n"));
        }
    }
    if let Some(feedback) = &inputs.revision_feedback {
        text.push_str(&format!(
            "\nRevision feedback from the developer: {}\n",
            feedback.trim()
        ));
    }
    for (question, answer) in &inputs.clarification_answers {
        text.push_str(&format!(
            "\nClarification - Q: {} A: {}\n",
            question.trim(),
            answer.trim()
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
                logical_unit_key: Handle::new("classify_batch").unwrap(),
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
                    text.push_str("\nIssues reported by Intake:\n");
                    for issue in &intake.issues {
                        text.push_str(&format!("- {issue}\n"));
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
                    logical_unit_key: Handle::new("specify_batch").unwrap(),
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
            text.push_str("\nApproved interface intent:\n");
            for intent in &spec.interface_intent {
                text.push_str(&format!("- {:?}: {}\n", intent.intent, intent.description));
            }
            if !spec.unit_proposals.is_empty() {
                text.push_str(
                    "\nApproved unit proposals (bind each `unitKey` in `unitBindings`):\n",
                );
                for proposal in &spec.unit_proposals {
                    text.push_str(&format!(
                        "- `{}` ({:?}): {}\n",
                        proposal.unit_key, proposal.kind, proposal.objective
                    ));
                }
            }
            extra = Some(text);
            vec![UnitDraft {
                    logical_unit_key: Handle::new("contract_batch").unwrap(),
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
                    logical_unit_key: Handle::new("implement_batch").unwrap(),
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
                    logical_unit_key: Handle::new("test_batch").unwrap(),
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
                plan_stage_units(
                    &mut builder,
                    &delivered,
                    inputs,
                    spec_record,
                    &proposals,
                    contract,
                    ExecutorKind::TestAuthor,
                    SourceRole::Source,
                )?
            }
        }
        Stage::Integrator => {
            let sources = add_sources(&mut builder, &inputs.sources, &mut delivered, true)?;
            let owned = sources
                .iter()
                .zip(&inputs.sources)
                .filter(|(_, source)| source.role == SourceRole::Document)
                .map(|(handle, _)| handle.clone())
                .collect::<Vec<_>>();
            extra = Some(batch_guidance(manifest, inputs));
            vec![UnitDraft {
                    logical_unit_key: Handle::new("review_batch").unwrap(),
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
    let table = builder.build();
    let mut drafts = drafts;
    for draft in &mut drafts {
        if draft.criterion_refs.is_empty() {
            draft.criterion_refs = all_criteria(&table);
        }
        if draft.task_refs.is_empty() {
            draft.task_refs = all_tasks(&table);
        }
    }
    Ok(StagePreparation {
        table,
        drafts,
        approved,
        delivered,
        requirements,
        decisions,
        interfaces,
        extra_instructions: extra,
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
            destinations.push(Destination {
                handle: current.clone(),
                path,
                role,
                baseline_bytes: delivered.get(&current).map(String::len),
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
            if builder
                .entry(&current)
                .is_some_and(ReferenceEntryV1::is_readable_input)
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
            let candidate = builder.add_candidate(
                read.path.clone(),
                baseline,
                read.producer_unit_id.clone(),
                read.role,
            )?;
            input_refs.push(candidate);
        }
        owned_all.extend(unit.owned_destination_refs.iter().cloned());
        drafts.push(UnitDraft {
            logical_unit_key: unit.key,
            executor_kind: kind,
            task_refs: unit.task_refs,
            criterion_refs: unit.criterion_refs,
            depends_on: unit.depends_on,
            input_refs,
            owned_destination_refs: unit.owned_destination_refs,
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
                replaceable: unit.owned_destination_refs.contains(handle)
                    && content.len() <= MAX_REPLACE_FILE_BYTES,
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
        decisions: &preparation.decisions,
        interfaces: &preparation.interfaces,
        delivered,
        dependency_outputs: Vec::new(),
        correction: None,
        extra_instructions: preparation.extra_instructions.as_deref(),
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
            let key = Handle::new("intake_note").unwrap();
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
            let key = Handle::new("specification_note").unwrap();
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
            let key = Handle::new("contract_document").unwrap();
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
                let report_key = Handle::new("test_report").unwrap();
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
                artifact_key: Handle::new("test_report").unwrap(),
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
            let key = Handle::new("integration_report").unwrap();
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
        if token.contains('/')
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

/// Parses the `FILES_NEW:`/`FILES_MOD:`/`FILES:` tag lines a developer (or,
/// from M5 onward, the Companion's synthesis prompt) may write at the start
/// of a clarification answer (ADR-022 clarification loop, P2/A2.2). Only the
/// leading block of tag lines is read -- scanning stops at the first line
/// that does not start with one of the three prefixes, so the prose that
/// follows is never touched. Values are comma-separated; a value that does
/// not parse as a `RelPath` is dropped rather than failing the whole answer,
/// since a malformed tag should mean "no extra anchor", never "reject the
/// developer's answer". `FILES:` (no suffix) is returned unresolved -- the
/// host decides new-vs-existing by checking the workspace, which this pure
/// function has no access to (see `submit_specification_answers`).
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
            break;
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
    // only the leading `FILES_*:` block of a clarification answer, leaving
    // the prose (including the raw tag lines) untouched for `batch_guidance`.
    #[test]
    fn clarification_tags_are_parsed_from_the_leading_lines_only() {
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
        };
        let doc = SourceFile {
            path: RelPath::new("docs/auth.md").unwrap(),
            role: SourceRole::Document,
            content: "# Auth\n".into(),
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
}
