//! Failure classification for the stage failure checkpoint (ADR-007 extension).
//!
//! A stage failure used to be absorbing: `PulsationStatus::Failed` has no outgoing
//! edge, so exhausting `runtime.max_retries` cost the whole pipeline. Recoverable
//! failures now open a `purpose='stage_failure'` checkpoint instead, and this module
//! decides which decisions that checkpoint may offer.
//!
//! The rule is narrow on purpose. Retry is offered only when repeating the same call
//! can plausibly produce a different result; revision only when a re-execution target
//! exists and human prose is the thing that changes the outcome. A failure whose
//! decision set collapses to abort-only opens no checkpoint at all: it stays terminal,
//! exactly as before.

use crate::llm::LlmErrorKind;
use crate::persistence_service::{PersistenceServiceError, PulsationStage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// `should_retry` would have said yes, but `attempts > max_retries`.
    RetryBudgetExhausted,
    /// Credentials, billing or routing: never auto-retried, fixable in Settings.
    ProviderConfiguration,
    /// Transport-level provider faults that are not auto-retried.
    ProviderTransport,
    /// Host storage/filesystem faults.
    Infrastructure,
    /// The executor died before the agent-run handler existed.
    ExecutorSetup,
    /// A host contract the model violated; a fresh identical sample would not fix it.
    HostContract,
    /// The agent itself reported a failed status update.
    ModelReportedFailure,
    /// The model ran out of output budget mid-answer. The process succeeded;
    /// the answer is simply incomplete. Re-running the identical call spends a
    /// full generation to truncate at the same place, so retry is never
    /// offered - what has to change is the amount of output asked for, or the
    /// backend that has to produce it.
    OutputTruncated,
    // --- Composed stages only (ADR-022). Never produced by v1-v3 paths. ---
    /// The shared stage budget has no permit left for another real invocation.
    BudgetExhausted,
    /// A host check failed or a semantic blocker remains after the bounded
    /// repair; production repair needs a human.
    ValidationFailed,
    /// A required check could not run (missing toolchain/profile).
    ValidationUnavailable,
    /// The worker diagnosed a conflict with an approved contract.
    ContractConflict,
    /// The plan or the required output does not fit the frozen envelope.
    CapacityExceeded,
    /// A worker tried to write outside its ownership.
    OwnershipViolation,
    /// A delivered input changed under the execution.
    StaleInput,
}

impl FailureClass {
    pub fn as_db_value(self) -> &'static str {
        match self {
            Self::RetryBudgetExhausted => "retry_budget_exhausted",
            Self::ProviderConfiguration => "provider_configuration",
            Self::ProviderTransport => "provider_transport",
            Self::Infrastructure => "infrastructure",
            Self::ExecutorSetup => "executor_setup",
            Self::HostContract => "host_contract",
            Self::ModelReportedFailure => "model_reported_failure",
            Self::OutputTruncated => "output_truncated",
            Self::BudgetExhausted => "budget_exhausted",
            Self::ValidationFailed => "validation_failed",
            Self::ValidationUnavailable => "validation_unavailable",
            Self::ContractConflict => "contract_conflict",
            Self::CapacityExceeded => "capacity_exceeded",
            Self::OwnershipViolation => "ownership_violation",
            Self::StaleInput => "stale_input",
        }
    }

    pub fn from_db_value(value: &str) -> Option<Self> {
        Some(match value {
            "retry_budget_exhausted" => Self::RetryBudgetExhausted,
            "provider_configuration" => Self::ProviderConfiguration,
            "provider_transport" => Self::ProviderTransport,
            "infrastructure" => Self::Infrastructure,
            "executor_setup" => Self::ExecutorSetup,
            "host_contract" => Self::HostContract,
            "model_reported_failure" => Self::ModelReportedFailure,
            "output_truncated" => Self::OutputTruncated,
            "budget_exhausted" => Self::BudgetExhausted,
            "validation_failed" => Self::ValidationFailed,
            "validation_unavailable" => Self::ValidationUnavailable,
            "contract_conflict" => Self::ContractConflict,
            "capacity_exceeded" => Self::CapacityExceeded,
            "ownership_violation" => Self::OwnershipViolation,
            "stale_input" => Self::StaleInput,
            _ => return None,
        })
    }

    /// Whether re-running the identical call is worth a token.
    pub fn offers_retry(self) -> bool {
        match self {
            Self::RetryBudgetExhausted
            | Self::ProviderConfiguration
            | Self::ProviderTransport
            | Self::Infrastructure
            | Self::ExecutorSetup => true,
            // The model already produced a durable `stage_acceptances` row, the
            // host rejected its output on a rule a resample would hit again, or
            // the same budget would cut the same answer at the same place.
            Self::HostContract | Self::ModelReportedFailure | Self::OutputTruncated => false,
            // Composed classes: an identical retry cannot buy a permit, run a
            // missing toolchain, change an approved contract, shrink the
            // envelope, widen ownership or refresh a fenced input. Retry is
            // offered only through `offered_decisions_v4` when an allowance
            // still exists (StaleInput re-reads the input under a new
            // generation).
            Self::BudgetExhausted
            | Self::ValidationFailed
            | Self::ValidationUnavailable
            | Self::ContractConflict
            | Self::CapacityExceeded
            | Self::OwnershipViolation => false,
            Self::StaleInput => true,
        }
    }

    /// Whether human prose attached as `revision_feedback` can change the outcome.
    /// Still gated by `failure_revision_target(stage)` having a target at all.
    pub fn offers_revision(self) -> bool {
        match self {
            Self::RetryBudgetExhausted
            | Self::HostContract
            | Self::ModelReportedFailure
            // Feedback is exactly the lever here: it can narrow the scope so
            // the answer fits the budget.
            | Self::OutputTruncated => true,
            // The model never produced output; feedback cannot supply an API key.
            Self::ProviderConfiguration
            | Self::ProviderTransport
            | Self::Infrastructure
            | Self::ExecutorSetup => false,
            // Composed classes recoverable through a human scope/contract/code
            // revision; a missing toolchain is fixed in settings, not by prose.
            Self::BudgetExhausted
            | Self::ValidationFailed
            | Self::ContractConflict
            | Self::CapacityExceeded
            | Self::OwnershipViolation
            | Self::StaleInput => true,
            Self::ValidationUnavailable => false,
        }
    }

    /// The `terminal_reason` written when the user aborts from this checkpoint.
    pub fn terminal_reason(self) -> &'static str {
        self.as_db_value()
    }
}

/// `None` means no checkpoint: keep today's terminal behaviour.
///
/// Cancellation is a human decision already, and CLI faults have their own older
/// checkpoint (`open_cli_fallback`), so both stay out.
pub fn classify_stage_failure(error: &PersistenceServiceError) -> Option<FailureClass> {
    Some(match error {
        PersistenceServiceError::Cancelled(_) => return None,
        // A truncated answer reaches here as a CLI fault, but "retry the CLI"
        // is precisely the wrong offer for it, so it does not opt out with the
        // rest of the CLI faults.
        PersistenceServiceError::Cli { code, .. }
            if matches!(
                code.as_str(),
                "cli_output_truncated" | "cli_output_budget_exceeded"
            ) =>
        {
            FailureClass::OutputTruncated
        }
        PersistenceServiceError::Cli { code, .. }
            if matches!(code.as_str(), "model_mismatch" | "model_unverified") =>
        {
            FailureClass::HostContract
        }
        PersistenceServiceError::Cli { .. } => return None,
        PersistenceServiceError::Retryable(_) | PersistenceServiceError::ModelOutputInvalid(_) => {
            FailureClass::RetryBudgetExhausted
        }
        PersistenceServiceError::Db(_)
        | PersistenceServiceError::Io(_)
        | PersistenceServiceError::Migration(_) => FailureClass::Infrastructure,
        PersistenceServiceError::Validation(_)
        | PersistenceServiceError::Runtime(_)
        | PersistenceServiceError::NotFound(_) => FailureClass::HostContract,
        PersistenceServiceError::Llm(error) => match error.kind {
            LlmErrorKind::Cancelled => return None,
            // The same set `should_retry` auto-retries: reaching here means the
            // budget ran out, not that the error is hopeless.
            LlmErrorKind::RateLimited
            | LlmErrorKind::Timeout
            | LlmErrorKind::Network
            | LlmErrorKind::ProviderUnavailable
            | LlmErrorKind::AllTargetsExhausted
            | LlmErrorKind::OutputSchema => FailureClass::RetryBudgetExhausted,
            LlmErrorKind::MissingApiKey
            | LlmErrorKind::Authentication
            | LlmErrorKind::Billing
            | LlmErrorKind::Permission
            | LlmErrorKind::ModelNotFound
            | LlmErrorKind::RouteConflict => FailureClass::ProviderConfiguration,
            LlmErrorKind::ModelMismatch | LlmErrorKind::ModelUnverified => {
                FailureClass::HostContract
            }
            LlmErrorKind::InvalidRequest
            | LlmErrorKind::InvalidResponse
            | LlmErrorKind::StreamFailed
            | LlmErrorKind::BackgroundDeferred
            | LlmErrorKind::Persistence => FailureClass::ProviderTransport,
        },
    })
}

/// A stable, machine-readable code for the failure, shown next to the diagnostic.
/// LLM faults reuse the router's own code so it matches the rest of the telemetry.
pub fn stage_failure_error_code(error: &PersistenceServiceError) -> String {
    match error {
        PersistenceServiceError::Llm(error) => error.kind.code().to_string(),
        PersistenceServiceError::ModelOutputInvalid(_) => "model_output_invalid".to_string(),
        PersistenceServiceError::Retryable(_) => "retryable".to_string(),
        PersistenceServiceError::Validation(_) => "validation".to_string(),
        PersistenceServiceError::Runtime(_) => "runtime".to_string(),
        PersistenceServiceError::NotFound(_) => "not_found".to_string(),
        PersistenceServiceError::Db(_) => "db".to_string(),
        PersistenceServiceError::Io(_) => "io".to_string(),
        PersistenceServiceError::Migration(_) => "migration".to_string(),
        PersistenceServiceError::Cli { code, .. } => code.clone(),
        PersistenceServiceError::Cancelled(_) => "cancelled".to_string(),
    }
}

/// Where a failure-driven revision re-enters the pipeline.
///
/// Deliberately separate from `revision_target`, which serves the HITL artifact
/// review and is only ever reached from stages that hold a checkpoint. A failure can
/// land on `Code`, and re-running `Code` with the developer's own guidance is exactly
/// the useful move there. Same-stage revision is already supported: Specification and
/// Contract do it today.
pub fn failure_revision_target(stage: PulsationStage) -> Option<PulsationStage> {
    match stage {
        PulsationStage::Specification | PulsationStage::Contract | PulsationStage::Code => {
            Some(stage)
        }
        PulsationStage::Test | PulsationStage::Integrator => Some(PulsationStage::Code),
        // Nothing precedes Intake, and it has no artifacts to supersede.
        PulsationStage::Intake => None,
    }
}

/// The decisions a composed stage failure offers (ADR-022). Retry is a real
/// invocation and therefore needs a remaining extra permit; revision targets
/// are the stage's own failure target plus, for a diagnosed approved-contract
/// conflict, the earlier Specification/Contract stages already approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedDecisionsV4 {
    pub class: FailureClass,
    pub allow_retry: bool,
    pub revision_targets: Vec<PulsationStage>,
}

impl OfferedDecisionsV4 {
    pub fn allow_revision(&self) -> bool {
        !self.revision_targets.is_empty()
    }

    /// The target a legacy `RequestRevision` decision resolves to: the
    /// stage's own failure target when offered, else the earliest permitted.
    pub fn primary_revision_stage(&self) -> Option<PulsationStage> {
        self.revision_targets.first().copied()
    }
}

/// `rejected_answer` marks a failure of the stage's own answer (malformed,
/// truncated, citing what it may not, writing outside its files). For Test
/// and the Integrator such an answer says nothing about the Code they
/// reviewed: the stage revises itself first, Code stays a second target, and
/// Retry is offered while the unit still holds an extra permit ("Teste Haiku
/// Only 1", 2026-09-24, re-ran a healthy Code for an Integrator slip).
pub fn offered_decisions_v4(
    class: FailureClass,
    stage: PulsationStage,
    remaining_extra_invocations: u32,
    rejected_answer: bool,
) -> Option<OfferedDecisionsV4> {
    let own_answer = rejected_answer
        && matches!(stage, PulsationStage::Test | PulsationStage::Integrator)
        && matches!(
            class,
            FailureClass::BudgetExhausted
                | FailureClass::OutputTruncated
                | FailureClass::OwnershipViolation
        );
    let allow_retry = match class {
        FailureClass::StaleInput => true,
        FailureClass::ProviderConfiguration
        | FailureClass::ProviderTransport
        | FailureClass::Infrastructure
        | FailureClass::ExecutorSetup
        | FailureClass::RetryBudgetExhausted => remaining_extra_invocations > 0,
        _ => own_answer && remaining_extra_invocations > 0,
    };
    let mut revision_targets = Vec::new();
    if own_answer {
        revision_targets.push(stage);
    }
    if class.offers_revision()
        && let Some(target) = failure_revision_target(stage)
        && !revision_targets.contains(&target)
    {
        revision_targets.push(target);
    }
    if class == FailureClass::ContractConflict {
        for earlier in [PulsationStage::Specification, PulsationStage::Contract] {
            if earlier < stage && !revision_targets.contains(&earlier) {
                revision_targets.push(earlier);
            }
        }
    }
    if !allow_retry && revision_targets.is_empty() {
        return None;
    }
    Some(OfferedDecisionsV4 {
        class,
        allow_retry,
        revision_targets,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferedDecisions {
    pub class: FailureClass,
    pub allow_retry: bool,
    pub allow_revision: bool,
    pub revision_stage: Option<PulsationStage>,
}

/// The decisions a checkpoint would offer, or `None` when the set collapses to abort
/// only. A checkpoint that can only be aborted is a dead end, so the caller must stay
/// terminal instead of parking the pulsation in `Checkpoint` with no way forward.
pub fn offered_decisions(class: FailureClass, stage: PulsationStage) -> Option<OfferedDecisions> {
    let revision_stage = class
        .offers_revision()
        .then(|| failure_revision_target(stage))
        .flatten();
    let allow_retry = class.offers_retry();
    if !allow_retry && revision_stage.is_none() {
        return None;
    }
    Some(OfferedDecisions {
        class,
        allow_retry,
        allow_revision: revision_stage.is_some(),
        revision_stage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmError;

    fn llm(kind: LlmErrorKind) -> PersistenceServiceError {
        PersistenceServiceError::Llm(LlmError::new(kind, "boom"))
    }

    #[test]
    fn cancellation_and_cli_never_open_a_checkpoint() {
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Cancelled("x".into())),
            None
        );
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Cli {
                code: "c".into(),
                message: "m".into()
            }),
            None
        );
        assert_eq!(classify_stage_failure(&llm(LlmErrorKind::Cancelled)), None);
    }

    #[test]
    fn classifies_every_llm_kind() {
        for (kind, expected) in [
            (
                LlmErrorKind::RateLimited,
                FailureClass::RetryBudgetExhausted,
            ),
            (LlmErrorKind::Timeout, FailureClass::RetryBudgetExhausted),
            (LlmErrorKind::Network, FailureClass::RetryBudgetExhausted),
            (
                LlmErrorKind::ProviderUnavailable,
                FailureClass::RetryBudgetExhausted,
            ),
            (
                LlmErrorKind::AllTargetsExhausted,
                FailureClass::RetryBudgetExhausted,
            ),
            (
                LlmErrorKind::OutputSchema,
                FailureClass::RetryBudgetExhausted,
            ),
            (
                LlmErrorKind::MissingApiKey,
                FailureClass::ProviderConfiguration,
            ),
            (
                LlmErrorKind::Authentication,
                FailureClass::ProviderConfiguration,
            ),
            (LlmErrorKind::Billing, FailureClass::ProviderConfiguration),
            (
                LlmErrorKind::Permission,
                FailureClass::ProviderConfiguration,
            ),
            (
                LlmErrorKind::ModelNotFound,
                FailureClass::ProviderConfiguration,
            ),
            (
                LlmErrorKind::RouteConflict,
                FailureClass::ProviderConfiguration,
            ),
            (
                LlmErrorKind::InvalidRequest,
                FailureClass::ProviderTransport,
            ),
            (
                LlmErrorKind::InvalidResponse,
                FailureClass::ProviderTransport,
            ),
            (LlmErrorKind::StreamFailed, FailureClass::ProviderTransport),
            (
                LlmErrorKind::BackgroundDeferred,
                FailureClass::ProviderTransport,
            ),
            (LlmErrorKind::Persistence, FailureClass::ProviderTransport),
        ] {
            assert_eq!(
                classify_stage_failure(&llm(kind)),
                Some(expected),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn classifies_host_error_variants() {
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::ModelOutputInvalid("x".into())),
            Some(FailureClass::RetryBudgetExhausted)
        );
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Retryable("x".into())),
            Some(FailureClass::RetryBudgetExhausted)
        );
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Validation("x".into())),
            Some(FailureClass::HostContract)
        );
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Runtime("x".into())),
            Some(FailureClass::HostContract)
        );
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::NotFound("x".into())),
            Some(FailureClass::HostContract)
        );
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Io(std::io::Error::other("x"))),
            Some(FailureClass::Infrastructure)
        );
    }

    #[test]
    fn error_codes_reuse_the_router_vocabulary() {
        assert_eq!(
            stage_failure_error_code(&llm(LlmErrorKind::Timeout)),
            "provider_timeout"
        );
        assert_eq!(
            stage_failure_error_code(&PersistenceServiceError::ModelOutputInvalid("x".into())),
            "model_output_invalid"
        );
    }

    #[test]
    fn db_round_trips_every_class() {
        for class in [
            FailureClass::RetryBudgetExhausted,
            FailureClass::ProviderConfiguration,
            FailureClass::ProviderTransport,
            FailureClass::Infrastructure,
            FailureClass::ExecutorSetup,
            FailureClass::HostContract,
            FailureClass::ModelReportedFailure,
            FailureClass::OutputTruncated,
            FailureClass::BudgetExhausted,
            FailureClass::ValidationFailed,
            FailureClass::ValidationUnavailable,
            FailureClass::ContractConflict,
            FailureClass::CapacityExceeded,
            FailureClass::OwnershipViolation,
            FailureClass::StaleInput,
        ] {
            assert_eq!(
                FailureClass::from_db_value(class.as_db_value()),
                Some(class)
            );
        }
        assert_eq!(FailureClass::from_db_value("nope"), None);
    }

    #[test]
    fn revision_targets_cover_every_stage() {
        use PulsationStage::*;
        assert_eq!(failure_revision_target(Intake), None);
        assert_eq!(failure_revision_target(Specification), Some(Specification));
        assert_eq!(failure_revision_target(Contract), Some(Contract));
        assert_eq!(failure_revision_target(Code), Some(Code));
        assert_eq!(failure_revision_target(Test), Some(Code));
        assert_eq!(failure_revision_target(Integrator), Some(Code));
    }

    #[test]
    fn abort_only_sets_open_no_checkpoint() {
        // The agent gave up at Intake: no retry (its acceptance is durable), and
        // nothing to revise. Must stay terminal instead of parking in Checkpoint.
        assert_eq!(
            offered_decisions(FailureClass::ModelReportedFailure, PulsationStage::Intake),
            None
        );
        assert_eq!(
            offered_decisions(FailureClass::HostContract, PulsationStage::Intake),
            None
        );
        // A provider fault at Intake still retries, so the checkpoint is useful.
        let intake = offered_decisions(FailureClass::ProviderTransport, PulsationStage::Intake)
            .expect("retry keeps the checkpoint alive");
        assert!(intake.allow_retry && !intake.allow_revision);
        assert_eq!(intake.revision_stage, None);
    }

    #[test]
    fn code_failure_offers_same_stage_revision() {
        let code = offered_decisions(FailureClass::ModelReportedFailure, PulsationStage::Code)
            .expect("Code has a revision target");
        assert!(!code.allow_retry);
        assert!(code.allow_revision);
        assert_eq!(code.revision_stage, Some(PulsationStage::Code));

        let test = offered_decisions(FailureClass::RetryBudgetExhausted, PulsationStage::Test)
            .expect("Test revises into Code");
        assert!(test.allow_retry && test.allow_revision);
        assert_eq!(test.revision_stage, Some(PulsationStage::Code));
    }

    #[test]
    fn provider_classes_never_offer_revision() {
        for class in [
            FailureClass::ProviderConfiguration,
            FailureClass::ProviderTransport,
            FailureClass::Infrastructure,
            FailureClass::ExecutorSetup,
        ] {
            let offered = offered_decisions(class, PulsationStage::Test).expect("retry");
            assert!(offered.allow_retry, "{class:?}");
            assert!(!offered.allow_revision, "{class:?}");
            assert_eq!(offered.revision_stage, None, "{class:?}");
        }
    }

    #[test]
    fn composed_failures_offer_retry_only_with_a_permit_and_gate_earlier_targets() {
        use PulsationStage::*;
        // Transport failure with a permit left: retry; without: revision only.
        let with = offered_decisions_v4(FailureClass::ProviderTransport, Code, 1, false).unwrap();
        assert!(with.allow_retry && !with.allow_revision());
        assert_eq!(
            offered_decisions_v4(FailureClass::ProviderTransport, Code, 0, false),
            None
        );
        let exhausted =
            offered_decisions_v4(FailureClass::BudgetExhausted, Code, 0, false).unwrap();
        assert!(!exhausted.allow_retry);
        assert_eq!(exhausted.revision_targets, vec![Code]);
        // A production defect found in Test routes to Code, never to Test itself.
        let defect = offered_decisions_v4(FailureClass::ValidationFailed, Test, 2, false).unwrap();
        assert!(!defect.allow_retry);
        assert_eq!(defect.revision_targets, vec![Code]);
        // A contract conflict in Code exposes the earlier approved stages as targets.
        let conflict =
            offered_decisions_v4(FailureClass::ContractConflict, Code, 2, false).unwrap();
        assert!(!conflict.allow_retry);
        assert_eq!(
            conflict.revision_targets,
            vec![Code, Specification, Contract]
        );
        // ...but not stages that come after the failing one.
        let early =
            offered_decisions_v4(FailureClass::ContractConflict, Specification, 0, false).unwrap();
        assert_eq!(early.revision_targets, vec![Specification]);
        // Missing toolchain: nothing a model or prose can fix; terminal.
        assert_eq!(
            offered_decisions_v4(FailureClass::ValidationUnavailable, Code, 2, false),
            None
        );
        // Intake has no revision target: budget exhaustion there is terminal.
        assert_eq!(
            offered_decisions_v4(FailureClass::BudgetExhausted, Intake, 0, false),
            None
        );
        assert!(
            offered_decisions_v4(FailureClass::StaleInput, Test, 0, false)
                .unwrap()
                .allow_retry
        );
    }

    #[test]
    fn a_rejected_answer_of_test_or_integrator_revises_the_stage_itself_first() {
        use PulsationStage::*;
        // "Teste Haiku Only 1": the Integrator's own answer was rejected and
        // the only offer was re-running the accepted Code.
        for stage in [Test, Integrator] {
            for class in [
                FailureClass::BudgetExhausted,
                FailureClass::OutputTruncated,
                FailureClass::OwnershipViolation,
            ] {
                let with = offered_decisions_v4(class, stage, 1, true).unwrap();
                assert!(with.allow_retry, "{stage:?} {class:?}");
                assert_eq!(with.revision_targets, vec![stage, Code], "{class:?}");
                assert_eq!(with.primary_revision_stage(), Some(stage));
                let spent = offered_decisions_v4(class, stage, 0, true).unwrap();
                assert!(!spent.allow_retry, "Retry needs the unit's extra permit");
                assert_eq!(spent.revision_targets, vec![stage, Code]);
            }
        }
        // A failed check is a defect of the reviewed code, not of the answer.
        let defect = offered_decisions_v4(FailureClass::ValidationFailed, Test, 1, true).unwrap();
        assert!(!defect.allow_retry);
        assert_eq!(defect.revision_targets, vec![Code]);
        // A diagnosis the worker reported still routes to Code.
        let reported =
            offered_decisions_v4(FailureClass::ModelReportedFailure, Integrator, 1, false).unwrap();
        assert!(!reported.allow_retry);
        assert_eq!(reported.revision_targets, vec![Code]);
        // Code revises itself already; the flag adds nothing there.
        let code = offered_decisions_v4(FailureClass::BudgetExhausted, Code, 1, true).unwrap();
        assert!(!code.allow_retry);
        assert_eq!(code.revision_targets, vec![Code]);
    }

    #[test]
    fn a_truncated_answer_offers_revision_and_never_an_identical_retry() {
        // The Integrator burned three full generations on this: two answers cut
        // off at the same output ceiling and one that spent the entire budget
        // on reasoning. "Retry the CLI" repeats it exactly.
        let error = PersistenceServiceError::Cli {
            code: "cli_output_truncated".into(),
            message: "OpenCode interrompeu a saída por limite de tokens (length)".into(),
        };
        assert_eq!(
            classify_stage_failure(&error),
            Some(FailureClass::OutputTruncated)
        );
        let offered = offered_decisions(FailureClass::OutputTruncated, PulsationStage::Integrator)
            .expect("truncation is recoverable through a revision");
        assert!(!offered.allow_retry);
        assert!(offered.allow_revision);
        assert_eq!(offered.revision_stage, Some(PulsationStage::Code));

        // Every other CLI fault keeps its own older checkpoint.
        assert_eq!(
            classify_stage_failure(&PersistenceServiceError::Cli {
                code: "cli_timeout".into(),
                message: "execução CLI excedeu o tempo limite".into(),
            }),
            None
        );
    }

    #[test]
    fn a_run_the_host_stopped_for_its_output_ceiling_is_classified_like_a_truncation() {
        let error = PersistenceServiceError::Cli {
            code: "cli_output_budget_exceeded".into(),
            message: "the CLI was stopped after producing 32624 output tokens".into(),
        };
        assert_eq!(
            classify_stage_failure(&error),
            Some(FailureClass::OutputTruncated)
        );
        assert_eq!(
            stage_failure_error_code(&error),
            "cli_output_budget_exceeded"
        );
    }
}
