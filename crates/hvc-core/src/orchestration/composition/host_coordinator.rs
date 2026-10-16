//! The composed stage coordinator (ADR-022, pipeline v4).
//!
//! Runs one stage job as a versioned recipe: gather exact inputs, freeze the
//! plan and its budget, execute work units under durable permits, validate
//! every child against the plan, aggregate deterministically and accept once
//! through the same transaction the legacy stage uses. Failures park the
//! stage on the existing failure checkpoint with the permitted decisions;
//! nothing here retries behind the budget or publishes a child early.
//!
//! Included into `persistence_service` so it can use the service's private
//! helpers. All model I/O happens outside database transactions.

use super::composition_persistence::check_status_of;
use super::*;
use crate::agent_runtime::{LlmCallOutput, dispatch_llm};
use crate::llm::{AgentExecutionTarget, LlmRequest};
use crate::orchestration::composition::aggregate::{self, LegacyAcceptance};
use crate::orchestration::composition::budget::{Pool, derive_budget};
use crate::orchestration::composition::plan::{PlanInputs, freeze_plan, repair_kind};
use crate::orchestration::composition::policy::{all_policies, common_rules, policy_for};
use crate::orchestration::composition::projection::{
    CorrectionContext, DeliveredSource, render_unit_prompt,
};
use crate::orchestration::composition::recipes::{self, SourceFile, StageInputs, StagePreparation};
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::schema::{
    RESPONSE_SCHEMA_NAME, SchemaFlavor, executor_response_schema,
};
use crate::orchestration::composition::validate::{PlanIndex, validate_child};
use crate::orchestration::composition::validation::profile::{
    ProfileOrigin, ValidationProfileSnapshot, applies_to_stage, resolve_profile,
};
use crate::orchestration::composition::validation::{
    Candidate, ReceiptContext, materialize_candidate, run_check,
};
use crate::orchestration::composition::{
    self as comp, AttemptState, CompositionError, ErrorCode, ExecutionStatus, ExecutorKind,
    Invariant, UnitModelResponseV1, UnitState, WorkerFailureCode,
};
use crate::orchestration::{
    ArtifactLanguageGuard, ContentSha256, ExecutionFence, ExpectedFileContent, FailureClass,
    StageHandoffV1, offered_decisions_v4,
};
use crate::repository_context::RepositoryContext;
use crate::usage::{UnitAttribution, attribute_execution_on};
use rusqlite::TransactionBehavior;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::composition_persistence::{
    AttemptRow, EffectiveProfile, ExecutionRow, UnitRow, composition_invalid,
};

/// Why a work unit did not reach `validated`.
enum UnitOutcome {
    Validated,
    NeedsInput(Vec<comp::QuestionV1>),
    /// The unit exhausted its own allowance; the driver parks the execution
    /// exactly once, after every sibling in flight has settled.
    Failed(Box<(UnitRow, UnitFailure)>),
}

/// The pool a first attempt prefers; the reservation transaction settles
/// the actual pool against the live ledger (a sibling may take the last
/// initial permit concurrently).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolPreference {
    /// An initial permit while the account still holds one, else extra.
    InitialThenExtra,
    Extra,
}

/// One dispatch's failure, classified for recovery.
struct UnitFailure {
    class: FailureClass,
    code: String,
    diagnostic: String,
    /// The worker could plausibly fix this with a bounded correction.
    repairable: bool,
    correction: Option<CorrectionContext>,
    /// The model's own answer was rejected (parse, validation, truncation):
    /// Test and the Integrator then revise themselves, not the Code.
    rejected_answer: bool,
}

/// A rejection of the worker's own response (parse or `validate_child`).
/// Every reference conflict there is the model citing a handle it may not
/// cite - a task, input, proposal key, anchor or check - and gets the bounded
/// correction; only a reference-table identity conflict (SC-003) is a host
/// invariant. Restricting the correction to SC-002/SC-004 turned an
/// Integrator's `checkRefs` slip into a forced Code revision ("Teste Haiku
/// Only 1", 2026-09-24). Host-side paths keep `classify_composition_error`.
fn classify_worker_rejection(error: &CompositionError) -> UnitFailure {
    let mut failure = classify_composition_error(error);
    failure.rejected_answer = true;
    if error.code == ErrorCode::ReferenceConflict && error.sc != Invariant::Sc003 {
        failure.class = FailureClass::BudgetExhausted;
        failure.repairable = true;
        failure.correction = Some(correction_for(error));
    }
    failure
}

fn correction_for(error: &CompositionError) -> CorrectionContext {
    CorrectionContext {
        diagnostics: vec![error.message.clone()],
        rejected_artifact_keys: error
            .related
            .iter()
            .filter(|handle| comp::references::Namespace::of(handle).is_none())
            .cloned()
            .collect(),
    }
}

fn classify_composition_error(error: &CompositionError) -> UnitFailure {
    let (class, repairable) = match error.code {
        ErrorCode::InvalidShape | ErrorCode::InvalidLanguage | ErrorCode::EvidenceUnresolved => {
            (FailureClass::BudgetExhausted, true)
        }
        ErrorCode::ReferenceConflict if matches!(error.sc, Invariant::Sc002 | Invariant::Sc004) => {
            // A model that cites an unassigned task or an undelivered input
            // can correct its own malformed envelope within the bounded
            // allowance. Path, candidate, ownership and content identity
            // conflicts are host invariants and remain non-repairable below.
            (FailureClass::BudgetExhausted, true)
        }
        ErrorCode::ReferenceConflict => (FailureClass::HostContract, false),
        ErrorCode::OwnershipViolation => (FailureClass::OwnershipViolation, true),
        ErrorCode::UnsupportedOperation => (FailureClass::HostContract, true),
        ErrorCode::OutputTruncated => (FailureClass::OutputTruncated, false),
        ErrorCode::ContractConflict => (FailureClass::ContractConflict, false),
        ErrorCode::ImplementationMismatch => (FailureClass::ValidationFailed, false),
        ErrorCode::ScopeInsufficient | ErrorCode::NeedsInput => {
            (FailureClass::ModelReportedFailure, false)
        }
        ErrorCode::ValidationFailed => (FailureClass::ValidationFailed, false),
        ErrorCode::ValidationUnavailable => (FailureClass::ValidationUnavailable, false),
        ErrorCode::BackendUnavailable | ErrorCode::TransportFailure => {
            (FailureClass::ProviderTransport, false)
        }
        ErrorCode::BudgetExhausted => (FailureClass::BudgetExhausted, false),
        ErrorCode::CapacityExceeded => (FailureClass::CapacityExceeded, false),
        ErrorCode::StaleInput => (FailureClass::StaleInput, false),
        ErrorCode::HostContractError
        | ErrorCode::Cancelled
        | ErrorCode::Superseded
        | ErrorCode::StaleResult => (FailureClass::HostContract, false),
    };
    UnitFailure {
        class,
        code: error.code.as_str().to_owned(),
        diagnostic: format!(
            "{} {}: {}",
            error.sc.as_str(),
            error.code.as_str(),
            error.message
        ),
        repairable,
        correction: repairable.then(|| correction_for(error)),
        rejected_answer: false,
    }
}

/// The route-level message alone ("all targets exhausted") hides why; the
/// per-attempt causes are what the developer needs to read at the checkpoint.
/// The units a round may dispatch together, in plan order (`units` is
/// loaded by ordinal): pending units whose every dependency is already
/// validated, capped at `max_in_flight`. Two units chosen this way never lie on a dependency
/// path of each other (an unvalidated intermediate would block the later
/// one) and never write the same file (the plan gives every destination
/// one owner), so their edits and candidates cannot interfere.
fn next_ready_units(units: &[UnitRow], max_in_flight: usize) -> Vec<&UnitRow> {
    let validated = units
        .iter()
        .filter(|unit| unit.state == UnitState::Validated)
        .map(|unit| unit.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut ready = units
        .iter()
        .filter(|unit| unit.state == UnitState::Pending)
        .filter(|unit| {
            unit.spec
                .dependencies
                .iter()
                .all(|dependency| validated.contains(dependency.as_str()))
        })
        .collect::<Vec<_>>();
    ready.truncate(max_in_flight.max(1));
    ready
}

fn transport_diagnostic(error: &PersistenceServiceError) -> String {
    match error {
        PersistenceServiceError::Llm(llm) if !llm.causes.is_empty() => {
            format!("{} Causas: {}", llm.message, llm.causes.join(" | "))
        }
        other => other.to_string(),
    }
}

fn classify_transport_error(error: &PersistenceServiceError) -> Option<UnitFailure> {
    if matches!(
        error,
        PersistenceServiceError::Cli { code, .. }
            if matches!(code.as_str(), "model_mismatch" | "model_unverified")
    ) {
        return Some(UnitFailure {
            class: FailureClass::HostContract,
            code: crate::orchestration::stage_failure_error_code(error),
            diagnostic: transport_diagnostic(error),
            repairable: false,
            correction: None,
            rejected_answer: false,
        });
    }
    if matches!(
        error,
        PersistenceServiceError::Cli { code, .. } if code == "cli_output_truncated"
    ) {
        return Some(UnitFailure {
            class: FailureClass::OutputTruncated,
            code: "output_truncated".into(),
            diagnostic: error.to_string(),
            repairable: false,
            correction: None,
            rejected_answer: true,
        });
    }
    // The host stopped a run that kept producing past its ceiling. Like a
    // truncation, the same prompt against the same ceiling spends it again, so
    // it is neither retried in place nor switched to another backend.
    if matches!(
        error,
        PersistenceServiceError::Cli { code, .. } if code == "cli_output_budget_exceeded"
    ) {
        return Some(UnitFailure {
            class: FailureClass::OutputTruncated,
            code: "output_budget_exceeded".into(),
            diagnostic: error.to_string(),
            repairable: false,
            correction: None,
            rejected_answer: true,
        });
    }
    let class = match error {
        PersistenceServiceError::Cli { .. } => FailureClass::ProviderTransport,
        other => crate::orchestration::classify_stage_failure(other)?,
    };
    Some(UnitFailure {
        class,
        code: crate::orchestration::stage_failure_error_code(error),
        diagnostic: transport_diagnostic(error),
        // A transport fault is retried once within the unit's allowance.
        repairable: matches!(
            class,
            FailureClass::ProviderTransport | FailureClass::RetryBudgetExhausted
        ),
        correction: None,
        rejected_answer: false,
    })
}

enum RepairOutcome {
    Validated,
    Failed(UnitFailure),
}

fn receipt_status_label(status: comp::ReceiptStatus) -> &'static str {
    match status {
        comp::ReceiptStatus::Passed => "passed",
        comp::ReceiptStatus::Failed => "failed",
        comp::ReceiptStatus::TimedOut => "timed out",
        comp::ReceiptStatus::Cancelled => "was cancelled",
        comp::ReceiptStatus::Unavailable => "is unavailable",
        comp::ReceiptStatus::NotRun => "did not run",
        comp::ReceiptStatus::Error => "hit a runner error",
    }
}

#[derive(serde::Deserialize)]
struct ContractedLiteralEntry {
    key: String,
    value: String,
}

#[derive(serde::Deserialize, Default)]
struct ContractFrontmatterFields {
    #[serde(rename = "type")]
    note_type: Option<String>,
    #[serde(rename = "contracted-literals", default)]
    contracted_literals: Vec<ContractedLiteralEntry>,
}

/// The YAML frontmatter block of a rendered vault note (between the two
/// `---` fences), if any. Not a general Markdown parser - just enough to
/// read the fixed shape `workspace_service`'s templates emit.
fn frontmatter_yaml(content: &str) -> Option<serde_yaml::Value> {
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    serde_yaml::from_str(&rest[..end]).ok()
}

/// Best-effort literal-value diff (recipes §Integrator): if a delivered
/// source is a rendered contract note (frontmatter `type: contract`), every
/// literal it lists under `contracted-literals` must appear verbatim in some
/// other delivered source. Contract-drift bugs on this project (wrong
/// localStorage keys, an inverted priority scale) went undetected because
/// nothing ever compared the contract's literal values against the code; a
/// contract note that documents none, or was never published as a file,
/// yields no findings here - this is additive to the reviewer's own reading,
/// never a blocker on its own.
fn contracted_literal_findings(sources: &[SourceFile]) -> Vec<String> {
    let mut findings = Vec::new();
    for (index, note) in sources.iter().enumerate() {
        let Some(yaml) = frontmatter_yaml(&note.content) else {
            continue;
        };
        let Ok(fields) = serde_yaml::from_value::<ContractFrontmatterFields>(yaml) else {
            continue;
        };
        if fields.note_type.as_deref() != Some("contract") {
            continue;
        }
        for literal in &fields.contracted_literals {
            if literal.value.is_empty() {
                continue;
            }
            let found_elsewhere = sources.iter().enumerate().any(|(other_index, other)| {
                other_index != index && other.content.contains(&literal.value)
            });
            if !found_elsewhere {
                findings.push(format!(
                    "Contracted literal `{}` = `{}` (from `{}`) was not found in any other delivered file.",
                    literal.key, literal.value, note.path
                ));
            }
        }
    }
    findings
}

/// `<script src="...">` targets named by one HTML source, in document order,
/// duplicates included. Not a general HTML parser - only reads the literal
/// `src="..."`/`src='...'` attribute of a `<script>` start tag.
fn extract_script_srcs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(tag_start) = rest.find("<script") {
        let tail = &rest[tag_start..];
        let Some(tag_end) = tail.find('>') else {
            break;
        };
        let tag = &tail[..tag_end];
        if let Some(after_src) = tag.split("src=").nth(1)
            && let Some(quote) = after_src.chars().next().filter(|c| *c == '"' || *c == '\'')
            && let Some(end) = after_src[1..].find(quote)
        {
            out.push(after_src[1..1 + end].to_string());
        }
        rest = &tail[tag_end + 1..];
    }
    out
}

/// Best-effort dead-code detection (recipes §Integrator), deliberately
/// narrow: for a simple no-bundler web project (an HTML entrypoint loading
/// plain `<script src="...">` files), a delivered `.js` source no HTML
/// entrypoint reaches is flagged as possibly unused. Only runs when at least
/// one `.html` source is present, so it never applies to a project with a
/// build step or that isn't browser-served at all - a real orphan (like
/// pulsation-2's `task.js`/`calendarSettings.js`, superseded in practice but
/// never wired up or removed) went unreported because the Integrator only
/// ever compares its own pulsation's files against each other.
fn unreachable_script_findings(sources: &[SourceFile]) -> Vec<String> {
    let html_sources: Vec<&SourceFile> = sources
        .iter()
        .filter(|s| s.path.as_str().to_ascii_lowercase().ends_with(".html"))
        .collect();
    if html_sources.is_empty() {
        return Vec::new();
    }
    let referenced: HashSet<String> = html_sources
        .iter()
        .flat_map(|html| extract_script_srcs(&html.content))
        .map(|src| src.trim_start_matches("./").to_ascii_lowercase())
        .collect();
    let html_paths = html_sources
        .iter()
        .map(|h| h.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    sources
        .iter()
        .filter(|s| s.path.as_str().to_ascii_lowercase().ends_with(".js"))
        .filter(|s| !referenced.contains(&s.path.as_str().to_ascii_lowercase()))
        .map(|s| {
            format!(
                "`{}` is delivered but no `<script src>` in {html_paths} reaches it - verify it is intentionally unused.",
                s.path
            )
        })
        .collect()
}

/// Build caches live under the data directory, keyed by toolchain, so a
/// candidate rebuild does not start from scratch and never touches the
/// project's own `target`.
fn cache_environment(
    candidates_root: &Path,
    snapshot: &ValidationProfileSnapshot,
) -> Vec<(String, String)> {
    if snapshot.profile.toolchain_identity == "cargo" {
        let dir = candidates_root
            .join("cache")
            .join(&snapshot.toolchain_sha256.as_str()[..16])
            .join("target");
        return vec![(
            "CARGO_TARGET_DIR".into(),
            dir.to_string_lossy().into_owned(),
        )];
    }
    Vec::new()
}

/// Maps a tool-reported path (relative to the candidate, or absolute inside
/// it, with either separator) to the plan handle of that file.
fn path_resolver<'a>(
    candidate_dir: &'a Path,
    preparation: &'a StagePreparation,
) -> impl Fn(&str) -> Option<comp::Handle> + 'a {
    move |reported: &str| {
        let normalized = reported.replace('\\', "/");
        let relative = Path::new(&normalized)
            .strip_prefix(candidate_dir)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or(normalized);
        let relative = relative.trim_start_matches("./");
        let path = comp::RelPath::new(relative).ok()?;
        preparation.table.destination_for_path(&path).cloned()
    }
}

fn flavor_for_provider(provider: &str) -> SchemaFlavor {
    if let Some(backend) = provider.strip_prefix("cli:") {
        return crate::terminals::CliBackend::parse(backend)
            .map(SchemaFlavor::for_cli_backend)
            .unwrap_or(SchemaFlavor::Canonical);
    }
    crate::llm::profile_by_id(provider)
        .map(|profile| SchemaFlavor::for_profile(&profile))
        .unwrap_or(SchemaFlavor::Canonical)
}

impl PersistenceService {
    /// Entry point from `execute_claimed_job` for pipeline v4 pulsations.
    pub(super) async fn execute_composed_stage(
        &self,
        job: &Job,
        parent_run_id: &str,
        stage: PulsationStage,
        current: &Pulsation,
        project: &Project,
        runtime_config: &AgentRuntimeConfig,
        cancellation: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let connection = self.connection()?;
        let fence = Self::execution_fence(&connection, &job.id)?;
        let handoff = self.load_stage_handoff(&fence, stage, usize::MAX)?;
        let root = Path::new(&project.workspace_path);
        let artifact_paths = Self::artifact_paths(&connection, &current.id)?;
        let settings = Self::composition_settings_for(&connection, &project.id)?;
        let profiles = Self::applicable_validation_profiles(
            &connection,
            &project.id,
            comp::Stage::from_pulsation(stage),
        )?;
        let mut inputs = Self::collect_stage_inputs(root, current, &handoff, &artifact_paths)?;
        inputs.checks = profiles
            .iter()
            .map(|profile| profile.check_ref.clone())
            .collect();
        inputs.qualified_recipes = settings.qualified_recipes.clone();
        drop(connection);

        let comp_stage = comp::Stage::from_pulsation(stage);
        let mut preparation = match recipes::prepare(comp_stage, &inputs) {
            Ok(preparation) => preparation,
            Err(error) => {
                let failure = classify_composition_error(&error);
                return self.park_without_execution(job, parent_run_id, stage, &failure);
            }
        };
        // Checks the approved specification made mandatory for a criterion
        // are required regardless of the profile's own flag.
        let promoted_checks = recipes::promoted_check_refs(&inputs);
        // Integrator: deterministic pre-checks first; the single reviewer
        // call addresses only what they leave open (recipes §Integrator).
        let host_findings = if comp_stage == comp::Stage::Integrator {
            let findings = self.integrator_prechecks(&current.id, &inputs, root)?;
            if !findings.is_empty() {
                let mut extra = preparation.extra_instructions.take().unwrap_or_default();
                extra.push_str("\n## Host pre-checks\n\nThe host verified these deterministically; address only what they leave open and never contradict them. They describe checks of earlier stages by name for context only: cite no check handle that is not listed under Host checks.\n");
                for finding in &findings {
                    extra.push_str("- ");
                    extra.push_str(finding);
                    extra.push('\n');
                }
                preparation.extra_instructions = Some(extra);
            }
            findings
        } else {
            Vec::new()
        };

        let admitted = self.admit_composed_execution(
            job,
            parent_run_id,
            stage,
            current,
            project,
            runtime_config,
            &fence,
            &preparation,
        )?;
        let Some(execution) = admitted else {
            return Ok(());
        };
        if cancellation.is_cancelled() {
            self.cancel_execution(&execution.id)?;
            return Err(PersistenceServiceError::Cancelled(
                "Composed run cancelled before dispatch.".into(),
            ));
        }

        let index = PlanIndex::new(&execution.plan);
        // Zero-call paths (recipes §Contract, §Test): when the approved
        // specification already settles the stage, the host synthesizes the
        // unit result, validates it like any child and spends no permit.
        self.apply_zero_call_units(&execution, &index, &inputs, &preparation)?;
        // Rounds of ready units (recipes §Grouping): the frozen plan caps how
        // many run at once; a round joins every unit it dispatched and its
        // outcomes are applied in ordinal order, never in arrival order.
        let max_in_flight = execution.plan.budget.max_in_flight_units.max(1) as usize;
        loop {
            let units = Self::load_units(&self.connection()?, &execution.id)?;
            let ready = next_ready_units(&units, max_in_flight);
            if ready.is_empty() {
                if units.iter().all(|unit| unit.state == UnitState::Validated) {
                    break;
                }
                return Err(PersistenceServiceError::Runtime(
                    "Composed run has no unit eligible to proceed.".into(),
                ));
            }
            let round = ready.into_iter().map(|unit| {
                self.run_unit(
                    job,
                    &execution,
                    &index,
                    &preparation,
                    unit,
                    project,
                    runtime_config,
                    cancellation,
                )
            });
            let outcomes = futures::future::join_all(round)
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(questions) = outcomes.iter().find_map(|outcome| match outcome {
                UnitOutcome::NeedsInput(questions) => Some(questions),
                _ => None,
            }) {
                return self.accept_composed_clarification(
                    job,
                    parent_run_id,
                    &handoff,
                    &execution,
                    &preparation,
                    questions,
                );
            }
            if let Some((unit, failure)) = outcomes.iter().find_map(|outcome| match outcome {
                UnitOutcome::Failed(failed) => Some((&failed.0, &failed.1)),
                _ => None,
            }) {
                self.park_execution(job, parent_run_id, &execution, Some(unit), failure)?;
                return Ok(());
            }
        }
        if let Some(failure) = Self::uncovered_criteria_failure(&self.connection()?, &execution)? {
            self.park_execution(job, parent_run_id, &execution, None, &failure)?;
            return Ok(());
        }
        // Every unit validated: run the host checks on the assembled
        // candidate (M4). A required failure either gets one bounded repair
        // or parks the stage; nothing here publishes.
        let Some((checks, receipt_ids)) = self
            .validate_stage(
                job,
                parent_run_id,
                &execution,
                &index,
                &preparation,
                &profiles,
                &promoted_checks,
                settings.validation_enabled,
                project,
                runtime_config,
                cancellation,
            )
            .await?
        else {
            return Ok(());
        };
        match self.finish_composed_stage(
            job,
            parent_run_id,
            &handoff,
            &execution,
            &inputs,
            &preparation,
            root,
            checks,
            receipt_ids,
            host_findings,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A host-side rejection after every child validated is a
                // contract defect, not a model fault: the execution ends
                // `failed` with the diagnostic and the legacy failure path
                // decides what the job offers.
                if !matches!(error, PersistenceServiceError::Cancelled(_)) {
                    let db = self.connection()?;
                    let _ = Self::transition_execution(
                        &db,
                        &execution.id,
                        &[
                            ExecutionStatus::Running,
                            ExecutionStatus::Validating,
                            ExecutionStatus::Ready,
                        ],
                        ExecutionStatus::Failed,
                        Some(("host_contract_error", &error.to_string())),
                    );
                }
                Err(error)
            }
        }
    }

    /// Synthesizes and accepts the units the approved specification already
    /// settles, without a model call or a permit (recipes §Contract, §Test):
    /// a Contract whose intents are all preserve/none with complete
    /// references, and a Test whose every criterion was approved
    /// `not_applicable`. The synthesized response passes the same validation
    /// as a model's and is aggregated the same way.
    fn apply_zero_call_units(
        &self,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        inputs: &StageInputs,
        preparation: &StagePreparation,
    ) -> Result<(), PersistenceServiceError> {
        let stage = comp::Stage::from_pulsation(execution.stage);
        let units = Self::load_units(&self.connection()?, &execution.id)?;
        for unit in units.into_iter().filter(|u| u.state == UnitState::Pending) {
            let Some(response) =
                recipes::zero_call_response(stage, inputs, preparation, &unit.spec)
            else {
                continue;
            };
            validate_child(index, &unit.spec, &preparation.approved, &response)
                .map_err(|e| composition_invalid(&e))?;
            let json = serde_json::to_string(&response)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let sha =
                comp::canonical_sha256(&response, &[]).map_err(|e| composition_invalid(&e))?;
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let rows = Self::load_units(&tx, &execution.id)?;
            let fresh = rows
                .iter()
                .find(|u| u.id == unit.id)
                .cloned()
                .ok_or_else(|| PersistenceServiceError::Runtime("Missing unit.".into()))?;
            let key = reuse_key(
                &execution.plan,
                &unit.spec,
                &dependency_digests(&unit.spec, &rows),
            );
            Self::accept_child(&tx, &fresh, None, &json, sha.as_str(), key.as_str(), None)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Deterministic Integrator pre-checks (recipes §Integrator): every
    /// criterion represented in the accepted Code evidence and Test coverage,
    /// and every required receipt of the accepted Code/Test executions still
    /// passed and fresh against the current baseline. Findings go to the
    /// reviewer's prompt and to the aggregate; the reviewer cannot pass a
    /// check by summarizing a report.
    fn integrator_prechecks(
        &self,
        pulsation_id: &str,
        inputs: &StageInputs,
        root: &Path,
    ) -> Result<Vec<String>, PersistenceServiceError> {
        let mut findings = recipes::criterion_representation_findings(inputs);
        findings.extend(contracted_literal_findings(&inputs.sources));
        findings.extend(unreachable_script_findings(&inputs.sources));
        let db = self.connection()?;
        let Some(data_dir) = self.database_path().parent().map(Path::to_path_buf) else {
            return Ok(findings);
        };
        let mut baseline: Option<Candidate> = None;
        for stage in [PulsationStage::Code, PulsationStage::Test] {
            let Some(execution) =
                Self::latest_execution_for_pulsation(&db, pulsation_id, Some(stage))?
            else {
                continue;
            };
            if execution.status != ExecutionStatus::Accepted {
                continue;
            }
            for check in &execution.plan.check_refs {
                let receipts = Self::latest_validation_receipts(&db, &execution.id)?;
                let Some(receipt) = receipts.iter().find(|r| &r.check_ref == check) else {
                    findings.push(format!(
                        "The {} check named {check} has no receipt (not run).",
                        stage.as_db_value()
                    ));
                    continue;
                };
                if receipt.status != comp::ReceiptStatus::Passed {
                    findings.push(format!(
                        "The {} check named {check} did not pass ({}).",
                        stage.as_db_value(),
                        receipt_status_label(receipt.status)
                    ));
                    continue;
                }
                // Freshness: the inputs the receipt proves must be the
                // inputs the workspace holds now.
                let Some(profile) =
                    Self::validation_profiles_for(&db, &self.project_of(pulsation_id)?)?
                        .into_iter()
                        .map(|(profile, _)| profile)
                        .find(|profile| &profile.check_ref == check)
                else {
                    continue;
                };
                let candidate = match &baseline {
                    Some(candidate) => candidate,
                    None => {
                        let dir = data_dir
                            .join("stage_candidates")
                            .join(pulsation_id)
                            .join("integrator");
                        match materialize_candidate(root, &[], &dir) {
                            Ok(candidate) => baseline.insert(candidate),
                            Err(_) => continue,
                        }
                    }
                };
                let current = candidate
                    .input_manifest_sha256(&profile.input_globs)
                    .map_err(|e| composition_invalid(&e))?;
                if current != receipt.input_manifest_sha256 {
                    findings.push(format!(
                        "The {} check named {check} is stale: its inputs changed after the receipt.",
                        stage.as_db_value()
                    ));
                }
            }
        }
        Ok(findings)
    }

    fn project_of(&self, pulsation_id: &str) -> Result<String, PersistenceServiceError> {
        Ok(self.connection()?.query_row(
            "SELECT project_id FROM pulsations WHERE id=?1",
            [pulsation_id],
            |r| r.get(0),
        )?)
    }

    /// Enabled validation profiles whose kind applies to `stage`.
    fn applicable_validation_profiles(
        db: &Connection,
        project_id: &str,
        stage: comp::Stage,
    ) -> Result<Vec<comp::ValidationCommandProfileV1>, PersistenceServiceError> {
        Ok(Self::validation_profiles_for(db, project_id)?
            .into_iter()
            .filter(|(profile, enabled)| *enabled && applies_to_stage(profile.kind, stage))
            .map(|(profile, _)| profile)
            .collect())
    }

    /// Every file the validated units of the execution produced, reconstructed
    /// over the frozen baseline (`path -> content`), in plan order.
    fn staged_outputs(
        &self,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        preparation: &StagePreparation,
    ) -> Result<Vec<(comp::RelPath, String)>, PersistenceServiceError> {
        let units = Self::load_units(&self.connection()?, &execution.id)?;
        let mut staged = Vec::new();
        for unit in units
            .iter()
            .filter(|unit| unit.state == UnitState::Validated)
        {
            let Some(json) = unit.validated_result_json.as_deref() else {
                continue;
            };
            let response: UnitModelResponseV1 = serde_json::from_str(json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let child = validate_child(index, &unit.spec, &preparation.approved, &response)
                .map_err(|e| composition_invalid(&e))?;
            for artifact in recipes::reconstruct_artifacts(&execution.plan, preparation, &child)
                .map_err(|e| composition_invalid(&e))?
            {
                if let Some(path) = artifact.relative_path {
                    staged.push((path, artifact.content));
                }
            }
        }
        Ok(staged)
    }

    /// Runs the applicable host checks against the assembled candidate and
    /// decides the stage's fate (validation contract §3): every required
    /// check must pass; a failure attributable to one unit's owned files gets
    /// one bounded repair by the unit's repair specialty, then the affected
    /// checks rerun; anything else parks the stage for a human. Returns the
    /// check summaries and receipt ids to aggregate, or `None` when parked.
    #[allow(clippy::too_many_arguments)]
    async fn validate_stage(
        &self,
        job: &Job,
        parent_run_id: &str,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        preparation: &StagePreparation,
        profiles: &[comp::ValidationCommandProfileV1],
        promoted_checks: &HashSet<comp::Handle>,
        validation_enabled: bool,
        project: &Project,
        runtime_config: &AgentRuntimeConfig,
        cancellation: &CancellationToken,
    ) -> Result<Option<(Vec<comp::CheckSummaryV1>, Vec<comp::Uuid>)>, PersistenceServiceError> {
        let required = |profile: &comp::ValidationCommandProfileV1| {
            profile.required || promoted_checks.contains(&profile.check_ref)
        };
        let db = self.connection()?;
        Self::transition_execution(
            &db,
            &execution.id,
            &[ExecutionStatus::Running],
            ExecutionStatus::Validating,
            None,
        )?;
        if profiles.is_empty() {
            return Ok(Some((Vec::new(), Vec::new())));
        }
        if !validation_enabled {
            // Configured but not enabled: the reports say `not_run`; the
            // human review proceeds, the qualification gate does not.
            return Ok(Some((
                profiles
                    .iter()
                    .map(|profile| comp::CheckSummaryV1 {
                        check_ref: profile.check_ref.clone(),
                        required: required(profile),
                        receipt_id: None,
                        status: comp::CheckStatus::NotRun,
                    })
                    .collect(),
                Vec::new(),
            )));
        }
        // Freeze the profiles for this execution: resolved executable identity
        // and digest, recorded as immutable snapshots.
        let mut snapshots: Vec<(comp::Uuid, ValidationProfileSnapshot)> = Vec::new();
        for profile in profiles {
            let origin = if profile.toolchain_identity == "cargo" {
                ProfileOrigin::Preset
            } else {
                ProfileOrigin::Custom
            };
            let snapshot = match resolve_profile(profile, origin) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    let failure = classify_composition_error(&error);
                    self.park_execution(job, parent_run_id, execution, None, &failure)?;
                    return Ok(None);
                }
            };
            let (snapshot_id, _) = Self::snapshot_policy(
                &db,
                "validation_profile",
                Some(&project.id),
                &serde_json::to_value(&snapshot)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
            )?;
            snapshots.push((
                comp::Uuid::new(snapshot_id).map_err(|e| composition_invalid(&e))?,
                snapshot,
            ));
        }
        // Every receipt row names the snapshot it ran under (`profile_snapshot_id`),
        // which is the durable record of the profile set of this execution.
        drop(db);
        let Some(data_dir) = self.database_path().parent().map(Path::to_path_buf) else {
            return Err(PersistenceServiceError::Runtime(
                "Data directory unavailable to materialize the candidate.".into(),
            ));
        };
        let root = Path::new(&project.workspace_path);
        let candidates_root = data_dir.join("stage_candidates");
        let build_dir = candidates_root.join(&execution.id).join("build");
        let mut repaired: HashSet<String> = HashSet::new();
        loop {
            if cancellation.is_cancelled() {
                self.cancel_execution(&execution.id)?;
                return Err(PersistenceServiceError::Cancelled(
                    "Composed run cancelled before validation.".into(),
                ));
            }
            let staged = self.staged_outputs(execution, index, preparation)?;
            let candidate = materialize_candidate(root, &staged, &build_dir)
                .map_err(|e| composition_invalid(&e))?;
            let mut summaries = Vec::with_capacity(snapshots.len());
            let mut receipts = Vec::with_capacity(snapshots.len());
            for (snapshot_id, snapshot) in &snapshots {
                let receipt = self
                    .run_or_reuse_check(
                        execution,
                        snapshot_id,
                        snapshot,
                        &candidate,
                        preparation,
                        &candidates_root,
                        cancellation,
                    )
                    .await?;
                summaries.push(comp::CheckSummaryV1 {
                    check_ref: snapshot.profile.check_ref.clone(),
                    required: required(&snapshot.profile),
                    receipt_id: Some(receipt.receipt_id.clone()),
                    status: check_status_of(receipt.status),
                });
                receipts.push((snapshot.clone(), receipt));
            }
            let failing = receipts
                .iter()
                .filter(|(snapshot, receipt)| {
                    required(&snapshot.profile) && receipt.status != comp::ReceiptStatus::Passed
                })
                .collect::<Vec<_>>();
            if failing.is_empty() {
                let ids = receipts.iter().map(|(_, r)| r.receipt_id.clone()).collect();
                return Ok(Some((summaries, ids)));
            }
            if failing
                .iter()
                .any(|(_, receipt)| receipt.status == comp::ReceiptStatus::Cancelled)
            {
                self.cancel_execution(&execution.id)?;
                return Err(PersistenceServiceError::Cancelled(
                    "Composed run cancelled during validation.".into(),
                ));
            }
            // A missing toolchain is not a model fault and no prose fixes it.
            if let Some((snapshot, receipt)) = failing.iter().find(|(_, receipt)| {
                matches!(
                    receipt.status,
                    comp::ReceiptStatus::Unavailable | comp::ReceiptStatus::Error
                )
            }) {
                let failure = UnitFailure {
                    class: FailureClass::ValidationUnavailable,
                    code: "validation_unavailable".into(),
                    diagnostic: format!(
                        "Check `{}` could not run ({}): {}",
                        snapshot.profile.check_ref,
                        receipt_status_label(receipt.status),
                        receipt
                            .diagnostics
                            .first()
                            .map(|d| d.message.as_str())
                            .unwrap_or("no diagnostic")
                    ),
                    repairable: false,
                    correction: None,
                    rejected_answer: false,
                };
                self.park_execution(job, parent_run_id, execution, None, &failure)?;
                return Ok(None);
            }
            // Attribution: every error diagnostic of every failing required
            // check must land in files one single unit owns, and none may be
            // a production defect surfaced by a test.
            let stage = comp::Stage::from_pulsation(execution.stage);
            let owner = self.attribute_failures(execution, &failing, stage)?;
            let can_repair = match &owner {
                Some(unit) if !repaired.contains(&unit.id) => {
                    let db = self.connection()?;
                    let used = Self::unit_extra_used(
                        &db,
                        &execution.budget_account_id,
                        &unit.logical_unit_key,
                    )?;
                    let remaining =
                        Self::load_ledger(&db, &execution.budget_account_id)?.remaining();
                    used == 0 && remaining.extra_invocations > 0
                }
                _ => false,
            };
            if can_repair {
                let unit = owner.expect("owner present");
                let diagnostics = failing
                    .iter()
                    .flat_map(|(snapshot, receipt)| {
                        receipt
                            .diagnostics
                            .iter()
                            .filter(|d| d.severity == comp::Severity::Error)
                            .map(move |d| format!("[{}] {}", snapshot.profile.check_ref, d.message))
                    })
                    .take(24)
                    .collect::<Vec<_>>();
                let outcome = self
                    .repair_unit_after_checks(
                        job,
                        execution,
                        index,
                        preparation,
                        &unit,
                        diagnostics,
                        project,
                        runtime_config,
                        cancellation,
                    )
                    .await?;
                repaired.insert(unit.id.clone());
                match outcome {
                    RepairOutcome::Validated => continue,
                    RepairOutcome::Failed(failure) => {
                        self.park_execution(job, parent_run_id, execution, Some(&unit), &failure)?;
                        return Ok(None);
                    }
                }
            }
            // No admissible repair: compare with the fenced baseline once so
            // the human sees whether the failure was already there.
            let (snapshot, receipt) = failing[0];
            let classification = self
                .classify_against_baseline(
                    execution,
                    &snapshots,
                    snapshot,
                    receipt,
                    root,
                    &candidates_root,
                    cancellation,
                )
                .await?;
            let failure = UnitFailure {
                class: FailureClass::ValidationFailed,
                code: "validation_failed".into(),
                diagnostic: format!(
                    "Check `{}` {} ({classification}). {}",
                    snapshot.profile.check_ref,
                    receipt_status_label(receipt.status),
                    receipt
                        .diagnostics
                        .iter()
                        .filter(|d| d.severity == comp::Severity::Error)
                        .take(8)
                        .map(|d| d.message.as_str())
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                repairable: false,
                correction: None,
                rejected_answer: false,
            };
            self.park_execution(job, parent_run_id, execution, owner.as_ref(), &failure)?;
            return Ok(None);
        }
    }

    /// The unit whose owned files explain every error diagnostic of the
    /// failing checks, when there is exactly one. Test failures reported by a
    /// test run (`test_failed_*`) are production defects: never attributed to
    /// the test author, so a human decides (§3, never weaken a test).
    fn attribute_failures(
        &self,
        execution: &ExecutionRow,
        failing: &[&(ValidationProfileSnapshot, comp::ValidationReceiptV1)],
        stage: comp::Stage,
    ) -> Result<Option<UnitRow>, PersistenceServiceError> {
        let units = Self::load_units(&self.connection()?, &execution.id)?;
        let mut owner: Option<String> = None;
        for (_, receipt) in failing {
            for diagnostic in receipt
                .diagnostics
                .iter()
                .filter(|d| d.severity == comp::Severity::Error)
            {
                if stage == comp::Stage::Test && diagnostic.code.as_str().starts_with("test_failed")
                {
                    return Ok(None);
                }
                let Some(source) = &diagnostic.source_ref else {
                    return Ok(None);
                };
                let Some(unit) = execution
                    .plan
                    .units
                    .iter()
                    .find(|unit| unit.owned_destination_refs.contains(source))
                else {
                    return Ok(None);
                };
                match &owner {
                    Some(existing) if existing != unit.unit_id.as_str() => return Ok(None),
                    _ => owner = Some(unit.unit_id.as_str().to_owned()),
                }
            }
        }
        Ok(owner.and_then(|id| units.into_iter().find(|unit| unit.id == id)))
    }

    /// One bounded correction of a validated unit driven by host diagnostics:
    /// the unit reopens under a new generation, its repair specialty answers
    /// with the diagnostics in the correction section, and the result must
    /// pass the same validation as the original.
    #[allow(clippy::too_many_arguments)]
    async fn repair_unit_after_checks(
        &self,
        job: &Job,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        preparation: &StagePreparation,
        unit: &UnitRow,
        diagnostics: Vec<String>,
        project: &Project,
        runtime_config: &AgentRuntimeConfig,
        cancellation: &CancellationToken,
    ) -> Result<RepairOutcome, PersistenceServiceError> {
        let reopened = {
            let db = self.connection()?;
            Self::transition_execution(
                &db,
                &execution.id,
                &[ExecutionStatus::Validating],
                ExecutionStatus::Running,
                None,
            )?;
            Self::reopen_unit_for_repair(&db, unit)?
        };
        let kind = repair_kind(reopened.executor_kind).unwrap_or(reopened.executor_kind);
        let correction = CorrectionContext {
            diagnostics,
            rejected_artifact_keys: Vec::new(),
        };
        let result = self
            .attempt_unit(
                job,
                execution,
                index,
                preparation,
                &reopened,
                kind,
                PoolPreference::Extra,
                Some(&correction),
                project,
                runtime_config,
                cancellation,
            )
            .await;
        let db = self.connection()?;
        let outcome = match result {
            Ok(AttemptResult::Validated) => RepairOutcome::Validated,
            Ok(AttemptResult::NeedsInput(_)) => RepairOutcome::Failed(UnitFailure {
                class: FailureClass::ValidationFailed,
                code: "validation_failed".into(),
                diagnostic:
                    "The repair specialty asked for input instead of correcting the diagnostics."
                        .into(),
                repairable: false,
                correction: None,
                rejected_answer: false,
            }),
            Ok(AttemptResult::Failed(mut failure)) => {
                failure.repairable = false;
                RepairOutcome::Failed(failure)
            }
            Err(PersistenceServiceError::Cancelled(message)) => {
                self.cancel_execution(&execution.id)?;
                return Err(PersistenceServiceError::Cancelled(message));
            }
            Err(error) => return Err(error),
        };
        Self::transition_execution(
            &db,
            &execution.id,
            &[ExecutionStatus::Running],
            ExecutionStatus::Validating,
            None,
        )?;
        Ok(outcome)
    }

    /// Runs a check or reuses a passed receipt bound to the same profile and
    /// exact input set (§2 reuse rule); persists every real run.
    #[allow(clippy::too_many_arguments)]
    async fn run_or_reuse_check(
        &self,
        execution: &ExecutionRow,
        snapshot_id: &comp::Uuid,
        snapshot: &ValidationProfileSnapshot,
        candidate: &Candidate,
        preparation: &StagePreparation,
        candidates_root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<comp::ValidationReceiptV1, PersistenceServiceError> {
        let db = self.connection()?;
        let input_manifest = candidate
            .input_manifest_sha256(&snapshot.profile.input_globs)
            .map_err(|e| composition_invalid(&e))?;
        if let Some(previous) = Self::find_validation_receipt(
            &db,
            &execution.id,
            snapshot.profile.check_ref.as_str(),
            input_manifest.as_str(),
            snapshot.profile_sha256.as_str(),
        )? && previous.status == comp::ReceiptStatus::Passed
        {
            return Ok(previous);
        }
        drop(db);
        let context = ReceiptContext {
            stage_execution_id: comp::Uuid::new(execution.id.clone())
                .map_err(|e| composition_invalid(&e))?,
            parent_generation: execution.parent_generation as u32,
            profile_snapshot_id: snapshot_id.clone(),
            extra_environment: cache_environment(candidates_root, snapshot),
        };
        let resolver = path_resolver(&candidate.dir, preparation);
        let receipt = run_check(snapshot, candidate, &context, &resolver, cancellation).await;
        Self::insert_validation_run(&self.connection()?, &receipt)?;
        Ok(receipt)
    }

    /// Runs the failing check once against the fenced baseline (no staged
    /// output) and classifies the candidate failure as `pre_existing`,
    /// `introduced` or `undetermined`. The baseline receipt is persisted and
    /// linked; a pre-existing failure is still a failure.
    #[allow(clippy::too_many_arguments)]
    async fn classify_against_baseline(
        &self,
        execution: &ExecutionRow,
        snapshots: &[(comp::Uuid, ValidationProfileSnapshot)],
        snapshot: &ValidationProfileSnapshot,
        receipt: &comp::ValidationReceiptV1,
        root: &Path,
        candidates_root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<&'static str, PersistenceServiceError> {
        let Some((snapshot_id, _)) = snapshots
            .iter()
            .find(|(_, s)| s.profile.check_ref == snapshot.profile.check_ref)
        else {
            return Ok("undetermined");
        };
        let baseline_dir = candidates_root.join(&execution.id).join("baseline");
        let baseline = match materialize_candidate(root, &[], &baseline_dir) {
            Ok(candidate) => candidate,
            Err(_) => return Ok("undetermined"),
        };
        let db = self.connection()?;
        let input_manifest = baseline
            .input_manifest_sha256(&snapshot.profile.input_globs)
            .map_err(|e| composition_invalid(&e))?;
        let existing = Self::find_validation_receipt(
            &db,
            &execution.id,
            snapshot.profile.check_ref.as_str(),
            input_manifest.as_str(),
            snapshot.profile_sha256.as_str(),
        )?;
        drop(db);
        let baseline_receipt = match existing {
            Some(previous) => previous,
            None => {
                let context = ReceiptContext {
                    stage_execution_id: comp::Uuid::new(execution.id.clone())
                        .map_err(|e| composition_invalid(&e))?,
                    parent_generation: execution.parent_generation as u32,
                    profile_snapshot_id: snapshot_id.clone(),
                    extra_environment: cache_environment(candidates_root, snapshot),
                };
                let resolver = |_: &str| -> Option<comp::Handle> { None };
                let mut baseline_receipt =
                    run_check(snapshot, &baseline, &context, &resolver, cancellation).await;
                baseline_receipt.baseline_receipt_id = Some(receipt.receipt_id.clone());
                Self::insert_validation_run(&self.connection()?, &baseline_receipt)?;
                baseline_receipt
            }
        };
        Ok(match baseline_receipt.status {
            comp::ReceiptStatus::Passed => "introduced",
            comp::ReceiptStatus::Failed => {
                let candidate_codes = receipt
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == comp::Severity::Error)
                    .map(|d| d.message.as_str())
                    .collect::<HashSet<_>>();
                let baseline_codes = baseline_receipt
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == comp::Severity::Error)
                    .map(|d| d.message.as_str())
                    .collect::<HashSet<_>>();
                if candidate_codes.is_subset(&baseline_codes) {
                    "pre_existing"
                } else {
                    "undetermined"
                }
            }
            _ => "undetermined",
        })
    }

    /// The exact bytes of every candidate this unit reads: the producer's
    /// validated response reconstructed over the frozen baseline. A producer
    /// that is not validated yet is a host error (units run in plan order).
    fn candidate_inputs(
        &self,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        preparation: &StagePreparation,
        spec: &comp::WorkUnitSpecV1,
    ) -> Result<Vec<(comp::Handle, String)>, PersistenceServiceError> {
        let mut delivered = Vec::new();
        let mut units: Option<Vec<UnitRow>> = None;
        for handle in &spec.input_refs {
            let Some(comp::ReferenceEntryV1::CandidateSource {
                relative_path,
                producer_unit_id,
                ..
            }) = preparation.table.entry(handle)
            else {
                continue;
            };
            let rows = match &units {
                Some(rows) => rows,
                None => units.insert(Self::load_units(&self.connection()?, &execution.id)?),
            };
            let producer = rows
                .iter()
                .find(|unit| unit.id == producer_unit_id.as_str())
                .filter(|unit| unit.state == UnitState::Validated)
                .ok_or_else(|| {
                    PersistenceServiceError::Validation(format!(
                        "SC-004: candidate `{handle}` cites a producer that is not validated yet."
                    ))
                })?;
            let json = producer.validated_result_json.as_deref().ok_or_else(|| {
                PersistenceServiceError::Validation(format!(
                    "SC-004: the producer of `{handle}` has no validated result."
                ))
            })?;
            let response: UnitModelResponseV1 = serde_json::from_str(json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let child = validate_child(index, &producer.spec, &preparation.approved, &response)
                .map_err(|e| composition_invalid(&e))?;
            let artifacts = recipes::reconstruct_artifacts(&execution.plan, preparation, &child)
                .map_err(|e| composition_invalid(&e))?;
            let key = relative_path.canonical_key();
            let content = artifacts
                .into_iter()
                .find(|artifact| {
                    artifact
                        .relative_path
                        .as_ref()
                        .is_some_and(|path| path.canonical_key() == key)
                })
                .map(|artifact| artifact.content)
                .ok_or_else(|| {
                    PersistenceServiceError::Validation(format!(
                        "SC-004: the producer of `{handle}` did not write `{relative_path}`."
                    ))
                })?;
            delivered.push((handle.clone(), content));
        }
        Ok(delivered)
    }

    /// Writes a validated unit's reconstructed files under
    /// `<data dir>/stage_candidates/<execution>/reconstructed/` with a
    /// manifest, outside the project workspace. Host checks (M4) materialize
    /// their candidate from here; publication never reads it.
    fn stage_candidate_files(
        &self,
        execution_id: &str,
        unit_key: &comp::Handle,
        artifacts: &[recipes::ReconstructedArtifact],
    ) -> Result<(), PersistenceServiceError> {
        let Some(data_dir) = self.database_path().parent() else {
            return Ok(());
        };
        let root = data_dir.join("stage_candidates").join(execution_id);
        let reconstructed = root.join("reconstructed");
        let mut manifest: Vec<serde_json::Value> =
            std::fs::read_to_string(root.join("manifest.json"))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
        for artifact in artifacts {
            let Some(path) = &artifact.relative_path else {
                continue;
            };
            let target =
                reconstructed.join(path.as_str().replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &artifact.content)?;
            manifest.retain(|entry| entry["path"].as_str() != Some(path.as_str()));
            manifest.push(serde_json::json!({
                "path": path.as_str(),
                "sha256": comp::Sha256Hex::of(artifact.content.as_bytes()),
                "producerUnit": unit_key,
            }));
        }
        manifest.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
        )?;
        Ok(())
    }

    /// `artifact_id -> relative_path` for the pulsation's published artifacts;
    /// the legacy handoff carries content but not the destination path.
    fn artifact_paths(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<HashMap<String, String>, PersistenceServiceError> {
        let mut statement = db.prepare(
            "SELECT id,relative_path FROM artifacts WHERE pulsation_id=?1 AND relative_path IS NOT NULL",
        )?;
        let rows = statement
            .query_map([pulsation_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Reads every exact input the recipe needs: frozen batch, prior typed
    /// payloads, baseline files (brief sources, explicit task paths, earlier
    /// published artifacts), clarification answers and revision feedback.
    fn collect_stage_inputs(
        root: &Path,
        current: &Pulsation,
        handoff: &StageHandoffV1,
        artifact_paths: &HashMap<String, String>,
    ) -> Result<StageInputs, PersistenceServiceError> {
        let manifest = handoff.batch_manifest.clone().ok_or_else(|| {
            PersistenceServiceError::Validation(
                "SC-002: a v4 pulsation requires a frozen task batch.".into(),
            )
        })?;
        let rows = handoff
            .artifacts
            .iter()
            .map(|artifact| (artifact.kind.clone(), artifact.content.clone()))
            .collect::<Vec<_>>();
        let prior = recipes::prior_payloads(&rows).map_err(|e| composition_invalid(&e))?;
        let repository = RepositoryContext::new(root).ok();
        let mut sources: Vec<SourceFile> = Vec::new();
        let mut seen = HashSet::new();
        let mut missing = Vec::new();
        let mut push = |path: comp::RelPath, content: String| {
            if seen.insert(path.canonical_key()) {
                let role = recipes::role_for_path(&path);
                sources.push(SourceFile {
                    path,
                    role,
                    content,
                });
            }
        };
        let read = |path: &str| -> Option<String> { repository.as_ref()?.read_document(path).ok() };
        for feature in &manifest.features {
            for source in &feature.brief.sources {
                let Ok(path) = comp::RelPath::new(source.relative_path.as_str()) else {
                    continue;
                };
                if let Some(content) = read(path.as_str()) {
                    push(path, content);
                }
            }
        }
        for task in &manifest.tasks {
            let text = format!("{} {}", task.draft.title, task.draft.description);
            for path in recipes::explicit_paths(&text) {
                match read(path.as_str()) {
                    Some(content) => push(path, content),
                    None => {
                        if !missing.contains(&path) {
                            missing.push(path);
                        }
                    }
                }
            }
        }
        // Clarification answers (ADR-022 loop, P2/A2.2) already carry
        // host-anchored paths (`submit_specification_answers` parses
        // `FILES_*:` tags and resolves a bare `FILES:` against the workspace
        // before persisting), so they feed the exact same `sources`/
        // `missing_paths` a task's own text would -- no separate field or
        // consumption path in `prepare()` is needed; the Specification recipe
        // (and Code/Test's ownership of missing paths) already reads these.
        for exchange in &handoff.clarifications {
            for answer in &exchange.answers {
                for raw in &answer.planned_paths {
                    if let Ok(path) = comp::RelPath::new(raw.as_str())
                        && !missing.contains(&path)
                    {
                        missing.push(path);
                    }
                }
                for raw in &answer.modified_paths {
                    let Ok(path) = comp::RelPath::new(raw.as_str()) else {
                        continue;
                    };
                    match read(path.as_str()) {
                        Some(content) => push(path, content),
                        None => {
                            if !missing.contains(&path) {
                                missing.push(path);
                            }
                        }
                    }
                }
            }
        }
        for artifact in &handoff.artifacts {
            let Some(relative) = artifact_paths.get(&artifact.artifact_id) else {
                continue;
            };
            let Ok(path) = comp::RelPath::new(relative.clone()) else {
                continue;
            };
            let content = read(path.as_str()).unwrap_or_else(|| artifact.content.clone());
            push(path, content);
        }
        missing.retain(|path| !seen.contains(&path.canonical_key()));
        let mut answers = Vec::new();
        for exchange in &handoff.clarifications {
            for answer in &exchange.answers {
                if let Some(question) = exchange
                    .questions
                    .iter()
                    .find(|question| question.id == answer.question_id)
                {
                    answers.push((question.question.clone(), answer.text_answer.clone()));
                }
            }
        }
        Ok(StageInputs {
            title: current.feature_name.clone(),
            manifest: Some(manifest),
            prior,
            sources,
            missing_paths: missing,
            checks: Vec::new(),
            revision_feedback: handoff.revision_feedback.clone(),
            clarification_answers: answers,
            qualified_recipes: Vec::new(),
        })
    }

    /// Resolves a role's frozen transport/model. A specialty may tune effort,
    /// but it cannot replace the canonical role's model or backend.
    fn resolve_effective_profile(
        db: &Connection,
        project_id: &str,
        pulsation_id: &str,
        role: AgentRole,
        kind: ExecutorKind,
        runtime_config: &AgentRuntimeConfig,
    ) -> Result<EffectiveProfile, PersistenceServiceError> {
        let api_override: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='api_direct')",
            params![pulsation_id, role.as_str()],
            |r| r.get(0),
        )?;
        let api_profile = |source: &str| EffectiveProfile {
            executor_kind: kind.as_str().to_owned(),
            transport: "api".into(),
            backend: None,
            requested_model: runtime_config.model.clone(),
            thinking_level: None,
            // Requested flavor; the flavor actually parsed follows the
            // provider observed on the answer (`flavor_for_provider`).
            schema_flavor: flavor_for_provider(runtime_config.provider.as_str()),
            attempt_limit: 1,
            source: source.to_owned(),
        };
        let specialty: Option<(String, Option<String>, Option<String>, String)> = db
            .query_row(
                "SELECT execution_mode,backend,model,thinking_level FROM composition_specialty_overrides WHERE project_id=?1 AND executor_kind=?2",
                params![project_id, kind.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let configured: Option<(String, String, Option<String>, String)> = db
            .query_row(
                "SELECT execution_mode,backend,model,thinking_level FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2",
                params![project_id, role.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let elected_backend: Option<String> = db
            .query_row(
                "SELECT backend FROM pulse_execution_overrides WHERE pulsation_id=?1 AND agent_role=?2 AND execution_mode='cli_process'",
                params![pulsation_id, role.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if api_override {
            if configured
                .as_ref()
                .is_some_and(|(mode, _, _, _)| mode != "api_direct")
            {
                return Err(PersistenceServiceError::Cli {
                    code: "model_mismatch".into(),
                    message: format!(
                        "API recovery override attempted to switch role `{}` away from its frozen CLI backend/model.",
                        role.as_str()
                    ),
                });
            }
            return Ok(api_profile("recovery_override"));
        }
        let Some((mode, backend, model, thinking)) = configured else {
            if specialty.is_some() {
                return Err(PersistenceServiceError::Cli {
                    code: "model_mismatch".into(),
                    message: format!(
                        "Specialty `{}` defines a transport/model without a canonical selection for role `{}`.",
                        kind.as_str(),
                        role.as_str()
                    ),
                });
            }
            return Ok(api_profile("stage_role_default"));
        };
        if elected_backend
            .as_deref()
            .is_some_and(|elected| elected != backend)
        {
            return Err(PersistenceServiceError::Cli {
                code: "model_mismatch".into(),
                message: format!(
                    "Pulsation fallback attempted to switch role `{}` from its frozen backend `{backend}` to `{}`.",
                    role.as_str(),
                    elected_backend.as_deref().unwrap_or_default()
                ),
            });
        }
        if let Some((specialty_mode, specialty_backend, specialty_model, specialty_thinking)) =
            specialty
        {
            let selected_backend = elected_backend.as_deref().unwrap_or(&backend);
            if specialty_mode != mode
                || specialty_backend
                    .as_deref()
                    .is_some_and(|value| value != selected_backend)
            {
                return Err(PersistenceServiceError::Cli {
                    code: "model_mismatch".into(),
                    message: format!(
                        "Specialty `{}` attempted to replace role `{}` transport/backend `{}/{};` its frozen selection must be inherited.",
                        kind.as_str(),
                        role.as_str(),
                        mode,
                        selected_backend
                    ),
                });
            }
            if let Some(specialty_model) = specialty_model {
                match crate::terminals::model_identity::verify_model(
                    Some(selected_backend),
                    model.as_deref(),
                    Some(&specialty_model),
                ) {
                    crate::terminals::model_identity::ModelVerification::Verified => {}
                    crate::terminals::model_identity::ModelVerification::Mismatch => {
                        return Err(PersistenceServiceError::Cli {
                            code: "model_mismatch".into(),
                            message: format!(
                                "Specialty `{}` selected `{}` but canonical role `{}` is frozen to `{}`.",
                                kind.as_str(),
                                specialty_model,
                                role.as_str(),
                                model.as_deref().unwrap_or("default")
                            ),
                        });
                    }
                    crate::terminals::model_identity::ModelVerification::Unverified => {
                        return Err(PersistenceServiceError::Cli {
                            code: "model_unverified".into(),
                            message: format!(
                                "Specialty `{}` cannot prove its model is the frozen selection for role `{}`.",
                                kind.as_str(),
                                role.as_str()
                            ),
                        });
                    }
                }
            }
            // Keep the selected canonical transport/model and allow only the
            // specialty's effort setting to vary.
            let backend = elected_backend.unwrap_or(backend);
            let flavor = crate::terminals::CliBackend::parse(&backend)
                .map(SchemaFlavor::for_cli_backend)
                .unwrap_or(SchemaFlavor::Canonical);
            return Ok(EffectiveProfile {
                executor_kind: kind.as_str().to_owned(),
                transport: if mode == "cli_process" { "cli" } else { "api" }.into(),
                backend: (mode == "cli_process").then_some(backend),
                requested_model: model
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| runtime_config.model.clone()),
                thinking_level: Some(specialty_thinking),
                schema_flavor: flavor,
                attempt_limit: 1,
                source: "stage_role_with_specialty_effort".into(),
            });
        }
        let source = "stage_role".to_owned();
        let backend = elected_backend.unwrap_or(backend);
        if mode != "cli_process" {
            return Ok(api_profile(&source));
        }
        let flavor = crate::terminals::CliBackend::parse(&backend)
            .map(SchemaFlavor::for_cli_backend)
            .unwrap_or(SchemaFlavor::Canonical);
        Ok(EffectiveProfile {
            executor_kind: kind.as_str().to_owned(),
            transport: "cli".into(),
            backend: Some(backend),
            requested_model: model
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "default".into()),
            thinking_level: Some(thinking),
            schema_flavor: flavor,
            attempt_limit: 1,
            source,
        })
    }

    /// Admission transaction: fences, superseding of a stale predecessor,
    /// policy snapshots, budget account, frozen plan and durable execution.
    /// Returns `None` when the stage was parked before any request.
    fn admit_composed_execution(
        &self,
        job: &Job,
        parent_run_id: &str,
        stage: PulsationStage,
        current: &Pulsation,
        project: &Project,
        runtime_config: &AgentRuntimeConfig,
        fence: &ExecutionFence,
        preparation: &StagePreparation,
    ) -> Result<Option<ExecutionRow>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_execution(&tx, fence)?;
        let pulse = fence.pulsation_id.as_deref().ok_or_else(|| {
            PersistenceServiceError::Validation("Stage without a pulsation.".into())
        })?;
        let input_revision = fence.input_revision.unwrap_or(1);
        if let Some(existing) = Self::find_execution_for_job(&tx, &job.id, fence.generation)? {
            if existing.status == ExecutionStatus::Accepted {
                tx.commit()?;
                return Ok(None);
            }
            return Err(PersistenceServiceError::Validation(
                "SC-015: a composed run already exists for this job generation.".into(),
            ));
        }
        let predecessor = Self::find_reusable_predecessor(&tx, pulse, stage, input_revision)?;
        if let Some(predecessor) = &predecessor {
            Self::transition_execution(
                &tx,
                &predecessor.id,
                &[
                    ExecutionStatus::Planned,
                    ExecutionStatus::Running,
                    ExecutionStatus::Validating,
                    ExecutionStatus::Ready,
                    ExecutionStatus::Parked,
                ],
                ExecutionStatus::Superseded,
                None,
            )?;
        }
        let settings = Self::composition_settings_for(&tx, &project.id)?;
        let recipe_snapshot = serde_json::json!({
            "recipe": comp::policy::RECIPE_POLICY_ID,
            "commonRules": common_rules(),
            "policies": all_policies(),
        });
        let (recipe_snapshot_id, policy_sha) =
            Self::snapshot_policy(&tx, "recipe", None, &recipe_snapshot)?;
        let _ = Self::snapshot_policy(
            &tx,
            "composition_settings",
            Some(&project.id),
            &serde_json::to_value(&settings)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
        )?;
        let role = crate::llm::AgentRole::for_stage(stage.as_db_value());
        let mut profile_snapshots = BTreeMap::new();
        for draft in &preparation.drafts {
            let mut kinds = vec![draft.executor_kind];
            if let Some(repair) = repair_kind(draft.executor_kind) {
                kinds.push(repair);
            }
            for kind in kinds {
                if profile_snapshots.contains_key(&kind) {
                    continue;
                }
                let profile = Self::resolve_effective_profile(
                    &tx,
                    &project.id,
                    pulse,
                    role,
                    kind,
                    runtime_config,
                )?;
                let (snapshot_id, _) = Self::snapshot_policy(
                    &tx,
                    "executor_profile",
                    Some(&project.id),
                    &serde_json::to_value(&profile)
                        .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
                )?;
                profile_snapshots.insert(
                    kind,
                    comp::Uuid::new(snapshot_id).map_err(|e| composition_invalid(&e))?,
                );
            }
        }
        let stage_output_limit = runtime_config.output_budget_for(role);
        let estimates = preparation
            .drafts
            .iter()
            .map(|draft| comp::budget::UnitEstimate {
                logical_unit_key: draft.logical_unit_key.clone(),
                estimated_output_tokens: draft.estimated_output_tokens,
            })
            .collect::<Vec<_>>();
        let (budget, _) = match derive_budget(
            stage_output_limit,
            runtime_config.context_chars_limit,
            &estimates,
            settings.max_in_flight_units,
        ) {
            Ok(budget) => budget,
            Err(error) => {
                let failure = classify_composition_error(&error);
                let parked = Self::park_in_tx_without_execution(
                    &tx,
                    job,
                    parent_run_id,
                    stage,
                    current,
                    &failure,
                )?;
                tx.commit()?;
                let _ = parked;
                return Ok(None);
            }
        };
        let account_id = Self::open_budget_account(
            &tx,
            pulse,
            stage,
            input_revision,
            &budget,
            policy_sha.as_str(),
        )?;
        let batch_sha = tx
            .query_row(
                "SELECT manifest_sha256 FROM pulsation_batches WHERE pulsation_id=?1",
                [pulse],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|sha| comp::Sha256Hex::new(sha).ok())
            .unwrap_or_else(|| comp::Sha256Hex::of(b""));
        let approved_contract_sha256 = preparation_contract_sha(preparation);
        let plan = match freeze_plan(PlanInputs {
            stage_execution_id: comp::Uuid::generate(),
            budget_account_id: comp::Uuid::new(account_id.clone())
                .map_err(|e| composition_invalid(&e))?,
            parent_fence: comp::ParentFenceV1 {
                project_id: comp::Uuid::new(fence.project_id.clone())
                    .map_err(|e| composition_invalid(&e))?,
                pulsation_id: comp::Uuid::new(pulse.to_owned())
                    .map_err(|e| composition_invalid(&e))?,
                input_revision: input_revision as u32,
                job_id: comp::Uuid::new(job.id.clone()).map_err(|e| composition_invalid(&e))?,
                generation: fence.generation as u32,
                state_version: fence.state_version as u32,
            },
            stage: comp::Stage::from_pulsation(stage),
            policy_sha256: policy_sha,
            batch_sha256: batch_sha,
            approved_contract_sha256,
            table: &preparation.table,
            units: preparation.drafts.clone(),
            stage_output_limit,
            context_chars_limit: runtime_config.context_chars_limit,
            max_in_flight_units: settings.max_in_flight_units,
            profile_snapshots,
        }) {
            Ok(plan) => plan,
            Err(error) => {
                let failure = classify_composition_error(&error);
                Self::park_in_tx_without_execution(
                    &tx,
                    job,
                    parent_run_id,
                    stage,
                    current,
                    &failure,
                )?;
                tx.commit()?;
                return Ok(None);
            }
        };
        Self::admit_execution(
            &tx,
            pulse,
            &job.id,
            &plan,
            &account_id,
            &recipe_snapshot_id,
            predecessor.as_ref().map(|p| p.id.as_str()),
        )?;
        Self::transition_execution(
            &tx,
            plan.stage_execution_id.as_str(),
            &[ExecutionStatus::Planned],
            ExecutionStatus::Running,
            None,
        )?;
        // Reuse validated predecessor units whose frozen inputs are identical
        // (SC-016). Units are visited in plan order, so a unit is reused only
        // when every dependency was itself reused with the same result digest;
        // a changed or missing dependency invalidates its descendants.
        if let Some(predecessor) = &predecessor {
            let previous = Self::load_units(&tx, &predecessor.id)?;
            let fresh = Self::load_units(&tx, plan.stage_execution_id.as_str())?;
            for unit in fresh {
                let Some(old) = previous.iter().find(|old| {
                    old.logical_unit_key == unit.logical_unit_key
                        && old.state == UnitState::Validated
                        && old.validated_result_json.is_some()
                }) else {
                    continue;
                };
                let current = Self::load_units(&tx, plan.stage_execution_id.as_str())?;
                let dependencies = dependency_digests(&unit.spec, &current);
                if dependencies.len() != unit.spec.dependencies.len() {
                    continue;
                }
                let old_key = old.reuse_key_sha256.clone().unwrap_or_default();
                let new_key = reuse_key(&plan, &unit.spec, &dependencies);
                if old_key != new_key.as_str() {
                    continue;
                }
                let Some(json) = old.validated_result_json.as_deref() else {
                    continue;
                };
                let Ok(response) = serde_json::from_str::<UnitModelResponseV1>(json) else {
                    continue;
                };
                if !matches!(response, UnitModelResponseV1::Completed { .. }) {
                    continue;
                }
                let reused_attempt: Option<String> = tx
                    .query_row(
                        "SELECT id FROM stage_work_unit_attempts WHERE unit_id=?1 AND state='succeeded' ORDER BY created_at DESC LIMIT 1",
                        [&old.id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .or_else(|| old.reused_from_attempt_id.clone());
                Self::accept_child(
                    &tx,
                    &unit,
                    None,
                    json,
                    old.validated_result_sha256.as_deref().unwrap_or(""),
                    new_key.as_str(),
                    reused_attempt.as_deref(),
                )?;
            }
        }
        let execution =
            Self::find_execution(&tx, plan.stage_execution_id.as_str())?.ok_or_else(|| {
                PersistenceServiceError::Runtime("Composed run not persisted.".into())
            })?;
        tx.commit()?;
        Ok(Some(execution))
    }

    fn park_without_execution(
        &self,
        job: &Job,
        parent_run_id: &str,
        stage: PulsationStage,
        failure: &UnitFailure,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pulse = job.pulsation_id.as_deref().ok_or_else(|| {
            PersistenceServiceError::Validation("Job without a pulsation.".into())
        })?;
        let current = Self::find_pulsation_by_id(&tx, pulse)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation missing.".into()))?;
        Self::park_in_tx_without_execution(&tx, job, parent_run_id, stage, &current, failure)?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }

    /// Parks before any execution exists (preparation or admission failed).
    /// Falls back to the legacy terminal failure when no decision is offered.
    fn park_in_tx_without_execution(
        tx: &rusqlite::Transaction<'_>,
        job: &Job,
        parent_run_id: &str,
        stage: PulsationStage,
        current: &Pulsation,
        failure: &UnitFailure,
    ) -> Result<bool, PersistenceServiceError> {
        let parked = Self::record_stage_failure_checkpoint(
            tx,
            current,
            stage,
            &job.id,
            Some(parent_run_id),
            failure.class,
            &failure.code,
            &failure.diagnostic,
        )?;
        if !parked {
            Self::finalize_composed_failure(tx, job, parent_run_id, current, failure)?;
        }
        Ok(parked)
    }

    fn finalize_composed_failure(
        tx: &rusqlite::Transaction<'_>,
        job: &Job,
        parent_run_id: &str,
        current: &Pulsation,
        failure: &UnitFailure,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        Self::transition_pulsation(
            tx,
            current,
            PulsationStatus::Failed,
            current.stage,
            "stage_failed",
            Some(&failure.diagnostic),
        )?;
        tx.execute(
            "UPDATE pulsations SET terminal_reason=?2 WHERE id=?1",
            params![current.id, failure.class.terminal_reason()],
        )?;
        tx.execute(
            "UPDATE jobs SET status='failed',last_error=?2,finished_at=?3,updated_at=?3 WHERE id=?1",
            params![job.id, failure.diagnostic, now],
        )?;
        tx.execute(
            "UPDATE agent_runs SET status='failed',updated_at=?2 WHERE id=?1",
            params![parent_run_id, now],
        )?;
        Self::mark_effects_for_review(tx, &current.id, "stage_failed")?;
        Self::enqueue_learning(tx, &current.id)?;
        Ok(())
    }

    /// Parks a running execution on the failure checkpoint with the v4
    /// decision set. Terminal when the class offers nothing.
    fn park_execution(
        &self,
        job: &Job,
        parent_run_id: &str,
        execution: &ExecutionRow,
        unit: Option<&UnitRow>,
        failure: &UnitFailure,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&tx, &execution.pulsation_id)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation missing.".into()))?;
        let ledger = Self::load_ledger(&tx, &execution.budget_account_id)?;
        let mut remaining = ledger.remaining();
        if let Some(unit) = unit {
            let used =
                Self::unit_extra_used(&tx, &execution.budget_account_id, &unit.logical_unit_key)?;
            remaining.extra_invocations =
                remaining.extra_invocations.min(1u32.saturating_sub(used));
        }
        let diagnostic = match unit {
            Some(unit) => format!("Unit `{}`: {}", unit.logical_unit_key, failure.diagnostic),
            None => failure.diagnostic.clone(),
        };
        let offered = offered_decisions_v4(
            failure.class,
            execution.stage,
            remaining.extra_invocations,
            failure.rejected_answer,
        );
        let parked = match &offered {
            Some(offered) => Self::record_stage_failure_checkpoint_v4(
                &tx,
                &current,
                execution.stage,
                &job.id,
                Some(parent_run_id),
                offered,
                &failure.code,
                &diagnostic,
                &execution.id,
                &remaining,
            )?,
            None => false,
        };
        let next = if parked {
            ExecutionStatus::Parked
        } else {
            ExecutionStatus::Failed
        };
        Self::transition_execution(
            &tx,
            &execution.id,
            &[
                ExecutionStatus::Planned,
                ExecutionStatus::Running,
                ExecutionStatus::Validating,
                ExecutionStatus::Ready,
            ],
            next,
            Some((&failure.code, &diagnostic)),
        )?;
        if !parked {
            let terminal = UnitFailure {
                class: failure.class,
                code: failure.code.clone(),
                diagnostic,
                repairable: false,
                correction: None,
                rejected_answer: failure.rejected_answer,
            };
            Self::finalize_composed_failure(&tx, job, parent_run_id, &current, &terminal)?;
        }
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }

    fn cancel_execution(&self, execution_id: &str) -> Result<(), PersistenceServiceError> {
        let db = self.connection()?;
        let _ = Self::transition_execution(
            &db,
            execution_id,
            &[
                ExecutionStatus::Planned,
                ExecutionStatus::Running,
                ExecutionStatus::Validating,
                ExecutionStatus::Ready,
            ],
            ExecutionStatus::Cancelled,
            Some(("cancelled", "The execution was cancelled.")),
        );
        Ok(())
    }

    /// A unit evidences only the criteria no sibling shares
    /// (`validate::exclusive_criteria`); the stage still owes evidence for
    /// every criterion. A shared criterion none of its units evidenced parks
    /// the stage before any host check runs.
    fn uncovered_criteria_failure(
        db: &Connection,
        execution: &ExecutionRow,
    ) -> Result<Option<UnitFailure>, PersistenceServiceError> {
        if execution.plan.units.len() < 2 {
            return Ok(None);
        }
        let responses = Self::load_units(db, &execution.id)?
            .into_iter()
            .filter_map(|unit| unit.validated_result_json)
            .filter_map(|json| serde_json::from_str::<UnitModelResponseV1>(&json).ok())
            .collect::<Vec<_>>();
        let mut evidenced = HashSet::new();
        for response in &responses {
            let UnitModelResponseV1::Completed { payload, .. } = response else {
                continue;
            };
            // Other stages judge their criteria per unit only.
            let Some(settled) = comp::validate::settled_criteria(payload) else {
                return Ok(None);
            };
            evidenced.extend(settled);
        }
        let uncovered = comp::validate::uncovered_stage_criteria(&execution.plan, &evidenced);
        if uncovered.is_empty() {
            return Ok(None);
        }
        let listed = uncovered
            .iter()
            .map(|(criterion, units)| {
                format!(
                    "`{criterion}` (units {})",
                    units
                        .iter()
                        .map(|unit| format!("`{unit}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Ok(Some(UnitFailure {
            class: FailureClass::HostContract,
            code: ErrorCode::EvidenceUnresolved.as_str().to_owned(),
            diagnostic: format!(
                "{} {}: no unit evidenced {listed}. A shared criterion needs evidence from at least one unit whose edits contribute to it.",
                Invariant::Sc010.as_str(),
                ErrorCode::EvidenceUnresolved.as_str()
            ),
            repairable: false,
            correction: None,
            rejected_answer: false,
        }))
    }

    /// Why the predecessor execution (a parked stage the user retried)
    /// rejected the last answer of this logical unit. Only rejections of the
    /// answer itself qualify; a transport fault says nothing to the model.
    fn carried_rejection(
        db: &Connection,
        execution: &ExecutionRow,
        unit: &UnitRow,
    ) -> Result<Option<CorrectionContext>, PersistenceServiceError> {
        let diagnostic: Option<String> = db
            .query_row(
                "SELECT a.diagnostic FROM stage_work_unit_attempts a JOIN stage_work_units u ON u.id=a.unit_id WHERE u.execution_id=(SELECT predecessor_execution_id FROM stage_executions WHERE id=?1) AND u.logical_unit_key=?2 AND a.state='rejected' AND a.diagnostic IS NOT NULL AND a.error_code IN ('invalid_shape','invalid_language','output_truncated','output_budget_exceeded','reference_conflict','ownership_violation','evidence_unresolved') ORDER BY a.created_at DESC,a.rowid DESC LIMIT 1",
                params![execution.id, unit.logical_unit_key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(diagnostic.map(|diagnostic| CorrectionContext {
            diagnostics: vec![diagnostic],
            rejected_artifact_keys: Vec::new(),
        }))
    }

    /// Runs one unit: initial attempt, then at most one bounded correction or
    /// transport retry within the shared allowance. Never parks: the driver
    /// does, once per execution, after the round settles.
    async fn run_unit(
        &self,
        job: &Job,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        preparation: &StagePreparation,
        unit: &UnitRow,
        project: &Project,
        runtime_config: &AgentRuntimeConfig,
        cancellation: &CancellationToken,
    ) -> Result<UnitOutcome, PersistenceServiceError> {
        let mut unit = unit.clone();
        let mut correction: Option<CorrectionContext> = None;
        // A Retry of a parked stage re-runs this unit in a fresh execution:
        // the first attempt carries why its last answer was rejected (never
        // the answer), or the Retry would resend the same prompt.
        let mut carried = if unit.generation == 1 {
            Self::carried_rejection(&self.connection()?, execution, &unit)?
        } else {
            None
        };
        let mut kind = unit.executor_kind;
        loop {
            if cancellation.is_cancelled() {
                self.cancel_execution(&execution.id)?;
                return Err(PersistenceServiceError::Cancelled(
                    "Composed run cancelled.".into(),
                ));
            }
            // The first attempt of a unit draws an initial permit while the
            // account still has one (a Retry generation shares the account,
            // so its units may already have to use the extra pool); any
            // correction draws from the extra pool. The reservation
            // transaction settles the pool against the live ledger.
            let pool = if unit.generation == 1 && correction.is_none() {
                PoolPreference::InitialThenExtra
            } else {
                PoolPreference::Extra
            };
            let attempt = match self
                .attempt_unit(
                    job,
                    execution,
                    index,
                    preparation,
                    &unit,
                    kind,
                    pool,
                    correction.as_ref().or(carried.as_ref()),
                    project,
                    runtime_config,
                    cancellation,
                )
                .await
            {
                Ok(AttemptResult::Validated) => return Ok(UnitOutcome::Validated),
                Ok(AttemptResult::NeedsInput(questions)) => {
                    return Ok(UnitOutcome::NeedsInput(questions));
                }
                Ok(AttemptResult::Failed(failure)) => failure,
                Err(PersistenceServiceError::Cancelled(message)) => {
                    self.cancel_execution(&execution.id)?;
                    return Err(PersistenceServiceError::Cancelled(message));
                }
                Err(error) => return Err(error),
            };
            carried = None;
            // One bounded correction/retry per unit, only for repairable
            // faults and only while the shared allowance permits it.
            let can_repair = attempt.repairable && unit.generation == 1;
            if can_repair {
                let db = self.connection()?;
                let used = Self::unit_extra_used(
                    &db,
                    &execution.budget_account_id,
                    &unit.logical_unit_key,
                )?;
                let remaining = Self::load_ledger(&db, &execution.budget_account_id)?.remaining();
                if used == 0 && remaining.extra_invocations > 0 {
                    let mut db = self.connection()?;
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let fresh = Self::load_units(&tx, &execution.id)?
                        .into_iter()
                        .find(|u| u.id == unit.id)
                        .ok_or_else(|| PersistenceServiceError::Runtime("Missing unit.".into()))?;
                    let next = Self::next_unit_generation(&tx, &fresh)?;
                    tx.commit()?;
                    unit = next;
                    kind = if attempt.correction.is_some() {
                        repair_kind(unit.executor_kind).unwrap_or(unit.executor_kind)
                    } else {
                        unit.executor_kind
                    };
                    correction = attempt.correction.clone().or_else(|| {
                        Some(CorrectionContext {
                            diagnostics: vec![format!(
                                "The previous invocation failed at the transport: {}",
                                attempt.diagnostic
                            )],
                            rejected_artifact_keys: Vec::new(),
                        })
                    });
                    continue;
                }
            }
            return Ok(UnitOutcome::Failed(Box::new((unit, attempt))));
        }
    }

    /// One reserved, dispatched, parsed and validated attempt of a unit.
    async fn attempt_unit(
        &self,
        job: &Job,
        execution: &ExecutionRow,
        index: &PlanIndex<'_>,
        preparation: &StagePreparation,
        unit: &UnitRow,
        kind: ExecutorKind,
        pool: PoolPreference,
        correction: Option<&CorrectionContext>,
        project: &Project,
        runtime_config: &AgentRuntimeConfig,
        cancellation: &CancellationToken,
    ) -> Result<AttemptResult, PersistenceServiceError> {
        let policy = policy_for(kind);
        // Validated predecessor outputs this unit reads (SC-004): reconstructed
        // from the persisted responses, never from the live workspace.
        let candidates = self.candidate_inputs(execution, index, preparation, &unit.spec)?;
        let mut prompt_inputs = recipes::prompt_inputs(preparation, &unit.spec);
        prompt_inputs.correction = correction;
        prompt_inputs.dependency_outputs = candidates
            .iter()
            .map(|(handle, content)| DeliveredSource {
                handle,
                content,
                replaceable: false,
            })
            .collect();
        let profile_binding = execution
            .plan
            .profile_bindings
            .iter()
            .find(|binding| binding.unit_id.as_str() == unit.id && binding.executor_kind == kind)
            .ok_or_else(|| {
                PersistenceServiceError::Validation(format!(
                    "SC-014: no frozen profile for `{}` in this run.",
                    kind.as_str()
                ))
            })?;
        let profile: EffectiveProfile = serde_json::from_value(Self::policy_snapshot_json(
            &self.connection()?,
            profile_binding.profile_snapshot_id.as_str(),
        )?)
        .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
        let prompt = render_unit_prompt(
            &execution.plan,
            &unit.spec,
            &preparation.table,
            policy,
            &prompt_inputs,
            profile.schema_flavor,
        );
        let response_schema = executor_response_schema(kind, profile.schema_flavor);
        let strict = executor_response_schema(kind, SchemaFlavor::Strict);
        let prompt_chars = (prompt.chars + response_schema.to_string().chars().count())
            .min(u32::MAX as usize) as u32;

        // Reservation + attempt in one transaction; nothing external yet.
        let (attempt, unit) = {
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let ordinal: u64 = tx.query_row(
                "SELECT COALESCE(MAX(ordinal),0)+1 FROM stage_work_unit_attempts WHERE unit_id=?1 AND unit_generation=?2",
                params![unit.id, unit.generation],
                |r| r.get(0),
            )?;
            let pool = match pool {
                PoolPreference::Extra => Pool::Extra,
                PoolPreference::InitialThenExtra => {
                    let remaining =
                        Self::load_ledger(&tx, &execution.budget_account_id)?.remaining();
                    if remaining.initial_invocations > 0 {
                        Pool::Initial
                    } else {
                        Pool::Extra
                    }
                }
            };
            let reservation = match Self::reserve_invocation(
                &tx,
                &execution.budget_account_id,
                &execution.id,
                unit,
                pool,
                ordinal,
                unit.spec.max_output_tokens,
                prompt_chars,
            ) {
                Ok(reservation) => reservation,
                Err(PersistenceServiceError::Validation(message)) => {
                    return Ok(AttemptResult::Failed(UnitFailure {
                        class: FailureClass::BudgetExhausted,
                        code: "budget_exhausted".into(),
                        diagnostic: message,
                        repairable: false,
                        correction: None,
                        rejected_answer: false,
                    }));
                }
                Err(error) => return Err(error),
            };
            let attempt = Self::create_attempt(
                &tx,
                &job.id,
                &execution.id,
                unit,
                ordinal,
                &reservation.id,
                kind,
                profile_binding.profile_snapshot_id.as_str(),
                &profile,
            )?;
            let fresh = Self::load_units(&tx, &execution.id)?
                .into_iter()
                .find(|u| u.id == unit.id)
                .ok_or_else(|| PersistenceServiceError::Runtime("Missing unit.".into()))?;
            Self::transition_unit(&tx, &fresh, UnitState::Running, None)?;
            let running = Self::load_units(&tx, &execution.id)?
                .into_iter()
                .find(|u| u.id == unit.id)
                .ok_or_else(|| PersistenceServiceError::Runtime("Missing unit.".into()))?;
            tx.commit()?;
            (attempt, running)
        };

        let role = crate::llm::AgentRole::for_stage(execution.stage.as_db_value());
        let request = LlmRequest {
            execution_id: crate::llm::ExecutionId::new(Uuid::new_v4().to_string()),
            generation_id: crate::llm::GenerationId::new(Uuid::new_v4().to_string()),
            project_id: project.id.clone(),
            pulsation_id: Some(execution.pulsation_id.clone()),
            job_id: Some(job.id.clone()),
            agent_run_id: Some(attempt.agent_run_id.clone()),
            job_chain: execution.pulsation_id.clone(),
            target: AgentExecutionTarget::ephemeral(
                format!("ephemeral:{}:{}", execution.id, unit.id),
                role,
            ),
            stage: execution.stage.as_db_value().to_string(),
            system_prompt: prompt.system.clone(),
            user_prompt: prompt.user.clone(),
            response_format: crate::llm::ResponseFormat::JsonSchema {
                name: RESPONSE_SCHEMA_NAME.to_string(),
                schema: response_schema.clone(),
            },
            max_output_tokens: unit.spec.max_output_tokens,
            temperature: runtime_config.temperature,
            deadline: std::time::Instant::now()
                + crate::agent_timeout::AgentTimeoutPolicy::for_role(role)
                    .base_timeout()
                    .max(std::time::Duration::from_millis(runtime_config.timeout_ms)),
            stream: true,
            legacy_provider: runtime_config.provider.as_str().to_string(),
            legacy_model: runtime_config.model.clone(),
            dispatch: crate::llm::DispatchPolicy {
                cli_timeout: Some(crate::agent_timeout::CliTimeoutBudget::for_role(
                    role,
                    runtime_config.timeout_ms,
                )),
                attempt_limit: Some(1),
                unit: Some(crate::llm::UnitDispatch {
                    stage_execution_id: execution.id.clone(),
                    unit_id: unit.id.clone(),
                    attempt_id: attempt.id.clone(),
                    executor_kind: kind.as_str().to_owned(),
                    frozen_transport: profile.transport.clone(),
                    frozen_backend: profile.backend.clone(),
                    frozen_model: profile.requested_model.clone(),
                }),
                strict_schema: Some(strict),
                host_validated: true,
            },
        };
        let dispatch_sha =
            comp::Sha256Hex::of(format!("{}\n{}", prompt.system, prompt.user).as_bytes());
        // The frozen spec plus the exact candidate bytes delivered to this attempt.
        let input_sha = comp::canonical_sha256(
            &serde_json::json!({
                "spec": comp::canonical_sha256(&unit.spec, &[]).map_err(|e| composition_invalid(&e))?,
                "candidates": candidates
                    .iter()
                    .map(|(handle, content)| (handle.as_str(), comp::Sha256Hex::of(content.as_bytes())))
                    .collect::<Vec<_>>(),
            }),
            &[],
        )
        .map_err(|e| composition_invalid(&e))?;
        if let Err(error) = Self::mark_attempt_started(
            &self.connection()?,
            &attempt,
            request.execution_id.as_str(),
            dispatch_sha.as_str(),
            input_sha.as_str(),
        ) {
            // Nothing left the host: the permit goes back to the pool and the
            // attempt closes without a charge.
            let db = self.connection()?;
            Self::release_reservation(&db, &attempt.reservation_id)?;
            Self::finish_attempt(
                &db,
                &attempt,
                AttemptState::Cancelled,
                Some("host_error"),
                Some(&error.to_string()),
                None,
                None,
            )?;
            return Err(error);
        }
        let attribution = UnitAttribution {
            stage_execution_id: execution.id.clone(),
            work_unit_id: unit.id.clone(),
            attempt_id: attempt.id.clone(),
            executor_kind: kind.as_str().to_owned(),
        };

        let terminal_service = self
            .terminal_service
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let dispatched = dispatch_llm(
            self.llm_router.as_ref(),
            terminal_service.as_ref(),
            &request,
            cancellation,
        )
        .await;
        let db = self.connection()?;
        let _ = attribute_execution_on(&db, request.execution_id.as_str(), &attribution);
        let output = match dispatched {
            Ok(output) => output,
            Err(error) => {
                let error = PersistenceServiceError::from(error);
                if matches!(error, PersistenceServiceError::Cancelled(_))
                    || matches!(&error, PersistenceServiceError::Llm(llm) if llm.kind == crate::llm::LlmErrorKind::Cancelled)
                {
                    Self::finish_attempt(
                        &db,
                        &attempt,
                        AttemptState::Cancelled,
                        Some("cancelled"),
                        Some("Cancelled."),
                        None,
                        None,
                    )?;
                    let _ = Self::transition_unit(
                        &db,
                        &unit,
                        UnitState::Cancelled,
                        Some(("cancelled", "Cancelled.")),
                    );
                    return Err(PersistenceServiceError::Cancelled(
                        "Composed run cancelled during the call.".into(),
                    ));
                }
                let failure = classify_transport_error(&error).unwrap_or(UnitFailure {
                    class: FailureClass::ProviderTransport,
                    code: crate::orchestration::stage_failure_error_code(&error),
                    diagnostic: error.to_string(),
                    repairable: false,
                    correction: None,
                    rejected_answer: false,
                });
                // Release the budget reservation for provider-configuration
                // errors (MissingApiKey, Authentication, Billing, Permission,
                // ModelNotFound, RouteConflict). These classes never reach the
                // provider, so no tokens were consumed and the invocation slot
                // should be returned to the pool for a corrected attempt.
                // ProviderTransport and RetryBudgetExhausted keep the settled
                // state because a real request may have left the host.
                let attempt_state = if failure.class == FailureClass::ProviderConfiguration {
                    Self::release_reservation(&db, &attempt.reservation_id)?;
                    AttemptState::Cancelled
                } else {
                    AttemptState::Rejected
                };
                Self::finish_attempt(
                    &db,
                    &attempt,
                    attempt_state,
                    Some(&failure.code),
                    Some(&failure.diagnostic),
                    None,
                    None,
                )?;
                Self::transition_unit(
                    &db,
                    &unit,
                    UnitState::Failed,
                    Some((&failure.code, &failure.diagnostic)),
                )?;
                return Ok(AttemptResult::Failed(failure));
            }
        };
        Self::persist_attempt_output(&db, &attempt, job, &output, &response_schema, None)?;
        let flavor = flavor_for_provider(&output.provider);
        let mut host_notes = Vec::new();
        let parsed = comp::parse::parse_unit_response(&output.raw_output, kind, flavor).and_then(
            |mut response| {
                // A stage without host checks has nothing a check citation
                // could name: the host clears such citations (with an audit
                // note) instead of failing the answer over them.
                if index.plan().check_refs.is_empty() {
                    host_notes = comp::validate::normalize_without_checks(&mut response);
                }
                validate_child(index, &unit.spec, &preparation.approved, &response)?;
                Ok(response)
            },
        );
        let host_note = (!host_notes.is_empty()).then(|| host_notes.join("; "));
        match parsed {
            Ok(response) => {
                if let UnitModelResponseV1::NeedsInput { questions, .. } = &response {
                    Self::finish_attempt(
                        &db,
                        &attempt,
                        AttemptState::Succeeded,
                        None,
                        None,
                        Some(&output.model),
                        None,
                    )?;
                    Self::transition_unit(
                        &db,
                        &unit,
                        UnitState::Failed,
                        Some(("needs_input", "The worker requested human input.")),
                    )?;
                    Self::persist_attempt_output(
                        &db,
                        &attempt,
                        job,
                        &output,
                        &response_schema,
                        None,
                    )?;
                    return Ok(AttemptResult::NeedsInput(questions.clone()));
                }
                if let UnitModelResponseV1::Failed {
                    code,
                    message,
                    related_refs,
                } = &response
                {
                    let error = CompositionError::new(
                        Invariant::Sc012,
                        match code {
                            WorkerFailureCode::ContractConflict => ErrorCode::ContractConflict,
                            WorkerFailureCode::ScopeInsufficient => ErrorCode::ScopeInsufficient,
                            WorkerFailureCode::ImplementationMismatch => {
                                ErrorCode::ImplementationMismatch
                            }
                            WorkerFailureCode::UnsupportedOperation => {
                                ErrorCode::UnsupportedOperation
                            }
                        },
                        message.clone(),
                    )
                    .with_related(related_refs.iter().cloned());
                    let mut failure = classify_composition_error(&error);
                    // A worker-reported failure is a diagnosis, not a malformed
                    // answer: it is never "repaired" by asking again.
                    failure.repairable = false;
                    failure.correction = None;
                    Self::finish_attempt(
                        &db,
                        &attempt,
                        AttemptState::Succeeded,
                        Some(&failure.code),
                        Some(&failure.diagnostic),
                        Some(&output.model),
                        None,
                    )?;
                    Self::transition_unit(
                        &db,
                        &unit,
                        UnitState::Failed,
                        Some((&failure.code, &failure.diagnostic)),
                    )?;
                    return Ok(AttemptResult::Failed(failure));
                }
                let json = serde_json::to_string(&response)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
                let sha =
                    comp::canonical_sha256(&response, &[]).map_err(|e| composition_invalid(&e))?;
                let mut db = self.connection()?;
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let units = Self::load_units(&tx, &execution.id)?;
                let fresh = units
                    .iter()
                    .find(|u| u.id == unit.id)
                    .cloned()
                    .ok_or_else(|| PersistenceServiceError::Runtime("Missing unit.".into()))?;
                let key = reuse_key(
                    &execution.plan,
                    &unit.spec,
                    &dependency_digests(&unit.spec, &units),
                );
                Self::accept_child(
                    &tx,
                    &fresh,
                    Some(&attempt.id),
                    &json,
                    sha.as_str(),
                    key.as_str(),
                    None,
                )?;
                Self::finish_attempt(
                    &tx,
                    &attempt,
                    AttemptState::Succeeded,
                    None,
                    host_note.as_deref(),
                    Some(&output.model),
                    Some(sha.as_str()),
                )?;
                tx.commit()?;
                // Stage the reconstructed files for host checks (M4) and
                // inspection; nothing reaches the live workspace here.
                if let Ok(child) =
                    validate_child(index, &unit.spec, &preparation.approved, &response)
                    && let Ok(artifacts) =
                        recipes::reconstruct_artifacts(&execution.plan, preparation, &child)
                {
                    let _ = self.stage_candidate_files(
                        &execution.id,
                        &unit.spec.logical_unit_key,
                        &artifacts,
                    );
                }
                Ok(AttemptResult::Validated)
            }
            Err(error) => {
                let failure = classify_worker_rejection(&error);
                Self::finish_attempt(
                    &db,
                    &attempt,
                    AttemptState::Rejected,
                    Some(&failure.code),
                    Some(&failure.diagnostic),
                    Some(&output.model),
                    None,
                )?;
                Self::transition_unit(
                    &db,
                    &unit,
                    UnitState::Failed,
                    Some((&failure.code, &failure.diagnostic)),
                )?;
                Self::persist_attempt_output(
                    &db,
                    &attempt,
                    job,
                    &output,
                    &response_schema,
                    Some(&failure.diagnostic),
                )?;
                Ok(AttemptResult::Failed(failure))
            }
        }
    }

    fn persist_attempt_output(
        db: &Connection,
        attempt: &AttemptRow,
        job: &Job,
        output: &LlmCallOutput,
        schema: &serde_json::Value,
        validation_error: Option<&str>,
    ) -> Result<(), PersistenceServiceError> {
        let structured = serde_json::from_str::<serde_json::Value>(&output.raw_output)
            .ok()
            .filter(|_| validation_error.is_none())
            .map(|value| value.to_string());
        let now = Self::now_iso();
        db.execute(
            "INSERT INTO agent_run_outputs(id,agent_run_id,job_id,pulsation_id,request_payload,response_payload,raw_output,structured_output,schema_json,validation_error,tool_trace,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL,?11,?11)
             ON CONFLICT(agent_run_id) DO UPDATE SET request_payload=excluded.request_payload,response_payload=excluded.response_payload,raw_output=excluded.raw_output,structured_output=excluded.structured_output,schema_json=excluded.schema_json,validation_error=excluded.validation_error,updated_at=excluded.updated_at",
            params![
                Uuid::new_v4().to_string(),
                attempt.agent_run_id,
                job.id,
                job.pulsation_id,
                output.request_payload.to_string(),
                output.response_payload.to_string(),
                output.raw_output,
                structured,
                schema.to_string(),
                validation_error,
                now
            ],
        )?;
        db.execute(
            "UPDATE agent_runs SET input_tokens=?2,output_tokens=?3,provider=?4,model=?5,llm_execution_id=?6,updated_at=?7 WHERE id=?1",
            params![
                attempt.agent_run_id,
                output.input_tokens,
                output.output_tokens,
                output.provider,
                output.model,
                output.execution_id,
                now
            ],
        )?;
        Ok(())
    }

    /// `needs_input` from Specification: the clarification checkpoint opens
    /// through the legacy acceptance path; the execution parks until the
    /// answers re-enqueue the stage.
    fn accept_composed_clarification(
        &self,
        job: &Job,
        parent_run_id: &str,
        handoff: &StageHandoffV1,
        execution: &ExecutionRow,
        preparation: &StagePreparation,
        questions: &[comp::QuestionV1],
    ) -> Result<(), PersistenceServiceError> {
        let legacy = clarification_with_task_ids(questions, &preparation.table);
        let outcome = crate::orchestration::StageOutcome::NeedsClarification {
            questions: legacy.clone(),
        };
        outcome
            .validate_with(execution.stage, ArtifactLanguageGuard::HostRendered)
            .map_err(|e| PersistenceServiceError::ModelOutputInvalid(e.to_string()))?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_execution(&tx, &handoff.recipient)?;
        let current = Self::find_pulsation_by_id(&tx, &execution.pulsation_id)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation missing.".into()))?;
        if current.status != PulsationStatus::Running || current.stage != execution.stage {
            return Err(PersistenceServiceError::Validation(
                "SC-015: stale run.".into(),
            ));
        }
        crate::feature_forge::pipeline::accept_evidence_rows(
            &tx,
            &execution.pulsation_id,
            &job.id,
            handoff.recipient.generation,
            execution.stage.as_db_value(),
            &crate::feature_forge::pipeline::EvidenceStatus::NeedsClarification {
                questions: legacy.iter().map(|q| q.question.clone()).collect(),
            },
            &[],
        )?;
        Self::transition_execution(
            &tx,
            &execution.id,
            &[ExecutionStatus::Running],
            ExecutionStatus::Parked,
            Some((
                "needs_input",
                "The specification author requested human input.",
            )),
        )?;
        Self::accept_stage_outcome_in_tx(
            &tx,
            job,
            parent_run_id,
            handoff,
            &current,
            &outcome,
            &super::serial::AcceptanceProvenance {
                stage_execution_id: Some(&execution.id),
                expected_overrides: None,
                language_guard: ArtifactLanguageGuard::HostRendered,
            },
            Path::new(""),
        )?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }

    /// All units validated: aggregate, lower to the legacy outcome and accept
    /// exactly once with the composed provenance.
    #[allow(clippy::too_many_arguments)]
    fn finish_composed_stage(
        &self,
        job: &Job,
        parent_run_id: &str,
        handoff: &StageHandoffV1,
        execution: &ExecutionRow,
        inputs: &StageInputs,
        preparation: &StagePreparation,
        root: &Path,
        checks: Vec<comp::CheckSummaryV1>,
        receipt_ids: Vec<comp::Uuid>,
        host_findings: Vec<String>,
    ) -> Result<(), PersistenceServiceError> {
        let db = self.connection()?;
        let units = Self::load_units(&db, &execution.id)?;
        let index = PlanIndex::new(&execution.plan);
        let stage = comp::Stage::from_pulsation(execution.stage);
        let mut responses = Vec::new();
        for unit in &units {
            let json = unit.validated_result_json.as_deref().ok_or_else(|| {
                PersistenceServiceError::Validation(format!(
                    "SC-010: unit `{}` has no validated result.",
                    unit.logical_unit_key
                ))
            })?;
            let response: UnitModelResponseV1 = serde_json::from_str(json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            responses.push((unit.clone(), response));
        }
        let mut validated = Vec::with_capacity(responses.len());
        for (unit, response) in &responses {
            let child = validate_child(&index, &unit.spec, &preparation.approved, response)
                .map_err(|e| composition_invalid(&e))?;
            let spec = execution
                .plan
                .units
                .iter()
                .find(|u| u.unit_id.as_str() == unit.id)
                .ok_or_else(|| PersistenceServiceError::Runtime("Unit fora do plano.".into()))?;
            validated.push((spec, child));
        }
        let refs = validated
            .iter()
            .map(|(spec, child)| (*spec, child))
            .collect::<Vec<_>>();
        let finish =
            recipes::finish_stage(stage, inputs, &execution.plan, preparation, &refs, &checks)
                .map_err(|e| composition_invalid(&e))?;
        let recipes::StageFinish {
            contributions,
            host_artifacts,
            payload_row,
            mut findings,
            summary,
        } = finish;
        for finding in host_findings {
            if !findings.contains(&finding) {
                findings.push(finding);
            }
        }
        let payload_rows = vec![payload_row];
        let aggregate = aggregate::build_aggregate(
            &execution.plan,
            &preparation.table,
            &contributions,
            host_artifacts,
            receipt_ids,
            findings,
        )
        .map_err(|e| composition_invalid(&e))?;
        let aggregate_sha =
            aggregate::aggregate_sha256(&aggregate).map_err(|e| composition_invalid(&e))?;
        let LegacyAcceptance { outcome, evidence } =
            aggregate::to_legacy(&aggregate, &preparation.table, summary, payload_rows)
                .map_err(|e| composition_invalid(&e))?;
        outcome
            .validate_with(execution.stage, ArtifactLanguageGuard::HostRendered)
            .map_err(|e| PersistenceServiceError::ModelOutputInvalid(e.to_string()))?;
        Self::transition_execution(
            &db,
            &execution.id,
            &[ExecutionStatus::Validating],
            ExecutionStatus::Ready,
            None,
        )?;
        drop(db);

        // Publication expects the bytes observed at admission: existing files
        // by hash, planned ones absent. A candidate entry describes an
        // intermediate version of one of those paths and adds no expectation.
        let mut expected = HashMap::new();
        for entry in &execution.plan.references {
            match entry {
                comp::ReferenceEntryV1::ExistingSource {
                    relative_path,
                    content_sha256,
                    ..
                } => {
                    expected.insert(
                        relative_path.as_str().to_ascii_lowercase(),
                        ExpectedFileContent::Present(ContentSha256::from(content_sha256)),
                    );
                }
                comp::ReferenceEntryV1::PlannedDestination { relative_path, .. } => {
                    expected.insert(
                        relative_path.as_str().to_ascii_lowercase(),
                        ExpectedFileContent::Absent,
                    );
                }
                comp::ReferenceEntryV1::CandidateSource { .. }
                | comp::ReferenceEntryV1::Unavailable { .. } => {}
            }
        }
        let artifact_count = match &outcome {
            crate::orchestration::StageOutcome::Completed { artifacts, .. } => artifacts.len(),
            _ => 0,
        };
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_execution(&tx, &handoff.recipient)?;
        let current = Self::find_pulsation_by_id(&tx, &execution.pulsation_id)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation missing.".into()))?;
        if current.status != PulsationStatus::Running || current.stage != execution.stage {
            return Err(PersistenceServiceError::Validation(
                "SC-015: stale run at acceptance.".into(),
            ));
        }
        crate::feature_forge::pipeline::accept_evidence_rows(
            &tx,
            &execution.pulsation_id,
            &job.id,
            handoff.recipient.generation,
            execution.stage.as_db_value(),
            &crate::feature_forge::pipeline::EvidenceStatus::Completed { artifact_count },
            &evidence,
        )?;
        let acceptance_id = Uuid::new_v4().to_string();
        Self::record_acceptance_on_execution(
            &tx,
            &execution.id,
            aggregate_sha.as_str(),
            &acceptance_id,
        )?;
        Self::accept_stage_outcome_in_tx(
            &tx,
            job,
            parent_run_id,
            handoff,
            &current,
            &outcome,
            &super::serial::AcceptanceProvenance {
                stage_execution_id: Some(&execution.id),
                expected_overrides: Some(&expected),
                language_guard: ArtifactLanguageGuard::HostRendered,
            },
            root,
        )?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }
}

enum AttemptResult {
    Validated,
    NeedsInput(Vec<comp::QuestionV1>),
    Failed(UnitFailure),
}

/// The reuse identity of a unit result: its frozen spec minus host-assigned
/// identifiers plus the exact delivered input hashes and the policy digest.
/// Validated result digests of a unit's dependencies within one execution,
/// in dependency order; a dependency without a validated result is absent.
fn dependency_digests(spec: &comp::WorkUnitSpecV1, units: &[UnitRow]) -> Vec<(String, String)> {
    spec.dependencies
        .iter()
        .filter_map(|dependency| {
            units
                .iter()
                .find(|unit| unit.id == dependency.as_str() && unit.state == UnitState::Validated)
                .and_then(|unit| {
                    unit.validated_result_sha256
                        .clone()
                        .map(|sha| (unit.logical_unit_key.as_str().to_owned(), sha))
                })
        })
        .collect()
}

fn reuse_key(
    plan: &comp::StageExecutionPlanV1,
    spec: &comp::WorkUnitSpecV1,
    dependency_digests: &[(String, String)],
) -> comp::Sha256Hex {
    let inputs = spec
        .input_refs
        .iter()
        .filter_map(|handle| {
            plan.references
                .iter()
                .find(|entry| entry.handle() == handle)
                .map(|entry| {
                    (
                        handle.as_str().to_owned(),
                        entry.relative_path().map(|p| p.as_str().to_owned()),
                        entry.content_sha256().map(|s| s.as_str().to_owned()),
                    )
                })
        })
        .collect::<Vec<_>>();
    let owned = spec
        .owned_destination_refs
        .iter()
        .filter_map(|handle| {
            plan.references
                .iter()
                .find(|entry| entry.handle() == handle)
                .and_then(|entry| entry.relative_path().map(|p| p.as_str().to_owned()))
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "policy": plan.policy_sha256,
        "batch": plan.batch_sha256,
        "contract": plan.approved_contract_sha256,
        "stage": plan.stage,
        "executorKind": spec.executor_kind,
        "logicalUnitKey": spec.logical_unit_key,
        "taskRefs": spec.task_refs,
        "criterionRefs": spec.criterion_refs,
        "objective": spec.objective,
        "inputs": inputs,
        "owned": owned,
        "checks": spec.required_check_refs,
        "dependencies": dependency_digests,
    });
    comp::canonical_sha256(&value, &[]).unwrap_or_else(|_| comp::Sha256Hex::of(b"unhashable"))
}

fn preparation_contract_sha(preparation: &StagePreparation) -> Option<comp::Sha256Hex> {
    if preparation.interfaces.is_empty() {
        return None;
    }
    comp::canonical_sha256(&preparation.interfaces, &[]).ok()
}

/// Legacy clarification questions must quote the affected task IDs literally.
fn clarification_with_task_ids(
    questions: &[comp::QuestionV1],
    table: &ReferenceTable,
) -> Vec<crate::orchestration::SpecificationQuestion> {
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let ids = question
                .task_refs
                .iter()
                .filter_map(|handle| table.task(handle).map(|t| t.task_id.clone()))
                .collect::<Vec<_>>();
            crate::orchestration::SpecificationQuestion {
                id: format!("q_{}", index + 1),
                question: format!("{} (tasks: {})", question.question.trim(), ids.join(", ")),
                rationale: question.rationale.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::composition::plan::{PlanInputs, UnitDraft, freeze_plan};
    use crate::orchestration::composition::references::ReferenceTable;
    use crate::orchestration::composition::{RelPath, SourceRole};
    use rusqlite::Connection;

    fn h(text: &str) -> comp::Handle {
        comp::Handle::new(text).unwrap()
    }

    fn source(path: &str, content: &str) -> SourceFile {
        SourceFile {
            path: RelPath::new(path).unwrap(),
            role: SourceRole::Source,
            content: content.into(),
        }
    }

    /// "Local Browser Architecture" / "Haiku CLI teste 1" (2026-09-09,
    /// 2026-09-26): the contract fixed `hvc_tasks` as the storage key, but a
    /// later, unrelated pulsation shipped `tasks_data` instead - nothing
    /// compared the two, so the divergence went unnoticed until a human read
    /// both documents side by side.
    #[test]
    fn contracted_literal_findings_flag_a_value_missing_from_every_other_file() {
        let contract = source(
            "knowledge/obsidian/contracts/local-browser.md",
            "---\ntype: contract\nproducers: []\nconsumers: []\naffects-features: []\nstatus: approved\ncontracted-literals:\n  - key: \"storage.tasks.key\"\n    value: \"hvc_tasks\"\n---\n\n# Local Browser\n",
        );
        let matching_code = source(
            "src/js/storage/localStore.js",
            "const STORAGE_KEY = 'hvc_tasks';\n",
        );
        let drifted_code = source(
            "src/js/storage/localStore.js",
            "const STORAGE_KEY = 'tasks_data';\n",
        );

        assert!(contracted_literal_findings(&[contract.clone(), matching_code]).is_empty());

        let findings = contracted_literal_findings(&[contract, drifted_code]);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("hvc_tasks"));
    }

    #[test]
    fn contracted_literal_findings_are_empty_without_a_published_contract_note() {
        let code = source(
            "src/js/app.js",
            "console.log('no contract note in context');\n",
        );
        assert!(contracted_literal_findings(&[code]).is_empty());
    }

    /// "Haiku CLI teste 1" (2026-09-26): `task.js`/`calendarSettings.js`
    /// (pulsation 2's deliverables) stayed on disk, fully accepted, but
    /// `index.html` never loaded them - the Integrator only ever compared its
    /// own pulsation's files against each other and never noticed.
    #[test]
    fn unreachable_script_findings_flag_a_js_file_no_entrypoint_loads() {
        let html = source(
            "index.html",
            "<html><body><script src=\"src/js/app.js\"></script></body></html>",
        );
        let wired = source("src/js/app.js", "// wired\n");
        let orphan = source("src/js/models/task.js", "// never loaded by index.html\n");

        assert!(unreachable_script_findings(&[html.clone(), wired.clone()]).is_empty());

        let findings = unreachable_script_findings(&[html, wired, orphan]);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("src/js/models/task.js"));
    }

    #[test]
    fn unreachable_script_findings_are_empty_without_any_html_entrypoint() {
        let js = source("src/lib.js", "// a library, not a browser entrypoint\n");
        assert!(unreachable_script_findings(&[js]).is_empty());
    }

    fn two_unit_plan() -> comp::StageExecutionPlanV1 {
        let mut builder = ReferenceTable::builder();
        builder
            .add_task(
                "t-1",
                1,
                "One",
                &[("AC-1".into(), "a".into()), ("AC-2".into(), "b".into())],
            )
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/a.rs").unwrap(),
                comp::Sha256Hex::of(b"a"),
                SourceRole::Source,
            )
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/b.rs").unwrap(),
                comp::Sha256Hex::of(b"b"),
                SourceRole::Source,
            )
            .unwrap();
        let table = builder.build();
        let draft = |key: &str, criterion: &str, owned: &str, depends: &[&str]| UnitDraft {
            logical_unit_key: h(key),
            executor_kind: ExecutorKind::CodeImplementer,
            task_refs: vec![h("task_1")],
            criterion_refs: vec![h(criterion)],
            depends_on: depends.iter().map(|d| h(d)).collect(),
            input_refs: vec![h(owned)],
            owned_destination_refs: vec![h(owned)],
            required_check_refs: vec![],
            objective: "Implement.".into(),
            estimated_output_tokens: 400,
            unit_id: None,
        };
        let mut snapshots = BTreeMap::new();
        snapshots.insert(ExecutorKind::CodeImplementer, comp::Uuid::generate());
        freeze_plan(PlanInputs {
            stage_execution_id: comp::Uuid::generate(),
            budget_account_id: comp::Uuid::generate(),
            parent_fence: comp::ParentFenceV1 {
                project_id: comp::Uuid::generate(),
                pulsation_id: comp::Uuid::generate(),
                input_revision: 1,
                job_id: comp::Uuid::generate(),
                generation: 1,
                state_version: 1,
            },
            stage: comp::Stage::Code,
            policy_sha256: comp::Sha256Hex::of(b"policy"),
            batch_sha256: comp::Sha256Hex::of(b"batch"),
            approved_contract_sha256: None,
            table: &table,
            units: vec![
                draft("producer", "crit_1_1", "src_1", &[]),
                draft("consumer", "crit_1_2", "src_2", &["producer"]),
            ],
            stage_output_limit: 8_000,
            context_chars_limit: 64_000,
            max_in_flight_units: 1,
            profile_snapshots: snapshots,
        })
        .unwrap()
    }

    fn row(spec: &comp::WorkUnitSpecV1, state: UnitState, sha: Option<&str>) -> UnitRow {
        UnitRow {
            id: spec.unit_id.as_str().to_owned(),
            logical_unit_key: spec.logical_unit_key.as_str().to_owned(),
            executor_kind: spec.executor_kind,
            spec: spec.clone(),
            generation: 1,
            state_version: 1,
            state,
            validated_result_json: sha.map(|_| "{}".to_owned()),
            validated_result_sha256: sha.map(str::to_owned),
            reused_from_attempt_id: None,
            reuse_key_sha256: None,
        }
    }

    fn profile_test_db(specialty_model: Option<&str>) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE pulse_execution_overrides(
               pulsation_id TEXT,agent_role TEXT,execution_mode TEXT,backend TEXT);
             CREATE TABLE composition_specialty_overrides(
               project_id TEXT,executor_kind TEXT,execution_mode TEXT,backend TEXT,model TEXT,thinking_level TEXT);
             CREATE TABLE cli_agent_configs(
               project_id TEXT,agent_role TEXT,execution_mode TEXT,backend TEXT,model TEXT,thinking_level TEXT);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO cli_agent_configs VALUES('project','backend_agent','cli_process','claude','haiku','medium')",
            [],
        )
        .unwrap();
        if let Some(model) = specialty_model {
            db.execute(
                "INSERT INTO composition_specialty_overrides VALUES('project','code_implementer','cli_process','claude',?1,'high')",
                [model],
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn specialty_inherits_canonical_model_and_rejects_conflicting_model() {
        let runtime = crate::agent_runtime::AgentRuntimeConfig::default();
        let role = crate::llm::AgentRole::BackendAgent;
        let same = profile_test_db(Some("haiku"));
        let profile = PersistenceService::resolve_effective_profile(
            &same,
            "project",
            "pulsation",
            role,
            ExecutorKind::CodeImplementer,
            &runtime,
        )
        .unwrap();
        assert_eq!(profile.backend.as_deref(), Some("claude"));
        assert_eq!(profile.requested_model, "haiku");
        assert_eq!(profile.source, "stage_role_with_specialty_effort");

        let conflicting = profile_test_db(Some("sonnet"));
        let error = PersistenceService::resolve_effective_profile(
            &conflicting,
            "project",
            "pulsation",
            role,
            ExecutorKind::CodeImplementer,
            &runtime,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PersistenceServiceError::Cli { ref code, .. } if code == "model_mismatch"
        ));

        let repair = PersistenceService::resolve_effective_profile(
            &same,
            "project",
            "pulsation",
            role,
            ExecutorKind::CodeRepairer,
            &runtime,
        )
        .unwrap();
        assert_eq!(repair.backend.as_deref(), Some("claude"));
        assert_eq!(repair.requested_model, "haiku");
    }

    #[test]
    fn canonical_roles_keep_independent_model_selections_for_code_and_intake() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE pulse_execution_overrides(
               pulsation_id TEXT,agent_role TEXT,execution_mode TEXT,backend TEXT);
             CREATE TABLE composition_specialty_overrides(
               project_id TEXT,executor_kind TEXT,execution_mode TEXT,backend TEXT,model TEXT,thinking_level TEXT);
             CREATE TABLE cli_agent_configs(
               project_id TEXT,agent_role TEXT,execution_mode TEXT,backend TEXT,model TEXT,thinking_level TEXT);
             INSERT INTO cli_agent_configs VALUES
               ('project','backend_agent','cli_process','codex','gpt-5.6-luna','medium'),
               ('project','intake_router','cli_process','claude','haiku','low');",
        )
        .unwrap();
        let runtime = crate::agent_runtime::AgentRuntimeConfig::default();
        let code = PersistenceService::resolve_effective_profile(
            &db,
            "project",
            "pulsation",
            crate::llm::AgentRole::BackendAgent,
            ExecutorKind::CodeImplementer,
            &runtime,
        )
        .unwrap();
        let repair = PersistenceService::resolve_effective_profile(
            &db,
            "project",
            "pulsation",
            crate::llm::AgentRole::BackendAgent,
            ExecutorKind::CodeRepairer,
            &runtime,
        )
        .unwrap();
        let intake = PersistenceService::resolve_effective_profile(
            &db,
            "project",
            "pulsation",
            crate::llm::AgentRole::IntakeRouter,
            ExecutorKind::IntakeClassifier,
            &runtime,
        )
        .unwrap();
        assert_eq!(
            (code.backend.as_deref(), code.requested_model.as_str()),
            (Some("codex"), "gpt-5.6-luna")
        );
        assert_eq!(
            (repair.backend.as_deref(), repair.requested_model.as_str()),
            (Some("codex"), "gpt-5.6-luna")
        );
        assert_eq!(
            (intake.backend.as_deref(), intake.requested_model.as_str()),
            (Some("claude"), "haiku")
        );
    }

    #[test]
    fn a_run_stopped_for_its_output_ceiling_rejects_the_answer_without_a_retry() {
        let error = PersistenceServiceError::Cli {
            code: "cli_output_budget_exceeded".into(),
            message: "the CLI was stopped after producing 32624 output tokens".into(),
        };
        let failure = classify_transport_error(&error).unwrap();
        assert_eq!(failure.class, FailureClass::OutputTruncated);
        assert_eq!(failure.code, "output_budget_exceeded");
        // The same prompt against the same ceiling spends it again.
        assert!(!failure.repairable);
        assert!(failure.rejected_answer);
    }

    #[test]
    fn real_reference_conflicts_are_host_contract_failures_not_budget_exhaustion() {
        let error = CompositionError::new(
            Invariant::Sc003,
            ErrorCode::ReferenceConflict,
            "same candidate path has different producer identity",
        );
        let failure = classify_composition_error(&error);
        assert_eq!(failure.class, FailureClass::HostContract);
        assert_eq!(failure.code, "reference_conflict");
        assert!(!failure.repairable);
        assert!(failure.correction.is_none());
    }

    #[test]
    fn model_citations_outside_the_unit_keep_the_bounded_correction_path() {
        let error = CompositionError::new(
            Invariant::Sc002,
            ErrorCode::ReferenceConflict,
            "Task `task_9` is not assigned to this unit.",
        );
        let failure = classify_composition_error(&error);
        assert_eq!(failure.class, FailureClass::BudgetExhausted);
        assert_eq!(failure.code, "reference_conflict");
        assert!(failure.repairable);
        assert!(failure.correction.is_some());
    }

    /// "Teste Haiku Only 1" (2026-09-24): an Integrator citing the test file
    /// `src_7` in `checkRefs` (SC-011) became a non-repairable host failure
    /// and forced a Code revision. A worker's citation of the wrong handle is
    /// correctable whatever the invariant; the table identity (SC-003) and
    /// host-side paths are not.
    #[test]
    fn a_worker_citation_error_of_any_invariant_gets_the_bounded_correction() {
        for (invariant, message) in [
            (
                Invariant::Sc011,
                "`assessment.checkRefs` cites unknown check `src_7`.",
            ),
            (
                Invariant::Sc006,
                "Proposal `view` depends on `plan_4`, which is not another proposal.",
            ),
            (
                Invariant::Sc009,
                "Anchor `anchor_9` was not issued in this plan.",
            ),
            (
                Invariant::Sc002,
                "Task `task_9` is not assigned to this unit.",
            ),
        ] {
            let error = CompositionError::new(invariant, ErrorCode::ReferenceConflict, message);
            let failure = classify_worker_rejection(&error);
            assert_eq!(
                failure.class,
                FailureClass::BudgetExhausted,
                "{invariant:?}"
            );
            assert!(failure.repairable, "{invariant:?}");
            let correction = failure.correction.expect("a correction");
            assert_eq!(correction.diagnostics, vec![message.to_string()]);
            assert!(failure.diagnostic.starts_with(invariant.as_str()));
        }
        let identity = CompositionError::new(
            Invariant::Sc003,
            ErrorCode::ReferenceConflict,
            "same candidate path has different producer identity",
        );
        let failure = classify_worker_rejection(&identity);
        assert_eq!(failure.class, FailureClass::HostContract);
        assert!(!failure.repairable && failure.correction.is_none());
        // Host-side paths are unchanged: an SC-011 raised outside a worker
        // response stays a host contract failure.
        let host = CompositionError::new(
            Invariant::Sc011,
            ErrorCode::ReferenceConflict,
            "Unit `review_batch` requires unknown check `cargo_check`.",
        );
        assert_eq!(
            classify_composition_error(&host).class,
            FailureClass::HostContract
        );
    }

    #[test]
    fn a_reuse_key_changes_with_a_dependency_result_and_ignores_unit_identity() {
        let plan = two_unit_plan();
        let consumer = &plan.units[1];
        let producer = &plan.units[0];
        let first = reuse_key(&plan, consumer, &[("producer".into(), "aaa".into())]);
        let same = reuse_key(&plan, consumer, &[("producer".into(), "aaa".into())]);
        let changed = reuse_key(&plan, consumer, &[("producer".into(), "bbb".into())]);
        assert_eq!(first, same);
        assert_ne!(
            first, changed,
            "a changed dependency result invalidates the descendant"
        );
        // A second plan with fresh unit ids but identical inputs keys the same.
        let again = two_unit_plan();
        assert_ne!(again.units[1].unit_id, consumer.unit_id);
        assert_eq!(
            reuse_key(
                &again,
                &again.units[1],
                &[("producer".into(), "aaa".into())]
            ),
            first
        );
        // Digests come only from validated dependencies.
        let rows = [
            row(producer, UnitState::Failed, None),
            row(consumer, UnitState::Pending, None),
        ];
        assert!(dependency_digests(consumer, &rows).is_empty());
        let rows = [
            row(producer, UnitState::Validated, Some("aaa")),
            row(consumer, UnitState::Pending, None),
        ];
        assert_eq!(
            dependency_digests(consumer, &rows),
            vec![("producer".to_owned(), "aaa".to_owned())]
        );
    }

    #[test]
    fn a_round_only_dispatches_pending_units_whose_dependencies_are_validated() {
        let plan = two_unit_plan();
        let producer = &plan.units[0];
        let consumer = &plan.units[1];
        // The consumer waits for the producer even with room for two.
        let rows = [
            row(producer, UnitState::Pending, None),
            row(consumer, UnitState::Pending, None),
        ];
        let ready = next_ready_units(&rows, 2);
        assert_eq!(
            ready
                .iter()
                .map(|u| u.logical_unit_key.as_str())
                .collect::<Vec<_>>(),
            ["producer"]
        );
        // Once the producer is validated the consumer becomes eligible; a
        // running or failed producer keeps it waiting.
        let rows = [
            row(producer, UnitState::Validated, Some("aaa")),
            row(consumer, UnitState::Pending, None),
        ];
        assert_eq!(next_ready_units(&rows, 2).len(), 1);
        assert_eq!(next_ready_units(&rows, 2)[0].logical_unit_key, "consumer");
        for blocked in [UnitState::Running, UnitState::Failed, UnitState::Cancelled] {
            let rows = [
                row(producer, blocked, None),
                row(consumer, UnitState::Pending, None),
            ];
            assert!(next_ready_units(&rows, 2).is_empty(), "{blocked:?}");
        }
        // Independent units share a round up to the cap, in plan order.
        let mut a = producer.clone();
        a.unit_id = comp::Uuid::generate();
        a.logical_unit_key = h("alpha");
        let mut b = producer.clone();
        b.unit_id = comp::Uuid::generate();
        b.logical_unit_key = h("beta");
        let mut c = producer.clone();
        c.unit_id = comp::Uuid::generate();
        c.logical_unit_key = h("gamma");
        let rows = [
            row(&a, UnitState::Pending, None),
            row(&b, UnitState::Pending, None),
            row(&c, UnitState::Pending, None),
        ];
        let keys = |cap: usize| {
            next_ready_units(&rows, cap)
                .iter()
                .map(|u| u.logical_unit_key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(1), ["alpha"]);
        assert_eq!(keys(2), ["alpha", "beta"]);
        assert_eq!(keys(0), ["alpha"], "the cap never drops below one");
        // Nothing pending: nothing dispatched.
        let rows = [row(&a, UnitState::Validated, Some("x"))];
        assert!(next_ready_units(&rows, 2).is_empty());
    }
}
