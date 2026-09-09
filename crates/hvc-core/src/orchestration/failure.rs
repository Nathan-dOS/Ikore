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
            // The model already produced a durable `stage_acceptances` row, or the
            // host rejected its output on a rule a resample would hit again.
            Self::HostContract | Self::ModelReportedFailure => false,
        }
    }

    /// Whether human prose attached as `revision_feedback` can change the outcome.
    /// Still gated by `failure_revision_target(stage)` having a target at all.
    pub fn offers_revision(self) -> bool {
        match self {
            Self::RetryBudgetExhausted | Self::HostContract | Self::ModelReportedFailure => true,
            // The model never produced output; feedback cannot supply an API key.
            Self::ProviderConfiguration
            | Self::ProviderTransport
            | Self::Infrastructure
            | Self::ExecutorSetup => false,
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
        PersistenceServiceError::Cancelled(_) | PersistenceServiceError::Cli { .. } => return None,
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
            (LlmErrorKind::RateLimited, FailureClass::RetryBudgetExhausted),
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
            (LlmErrorKind::Permission, FailureClass::ProviderConfiguration),
            (
                LlmErrorKind::ModelNotFound,
                FailureClass::ProviderConfiguration,
            ),
            (
                LlmErrorKind::RouteConflict,
                FailureClass::ProviderConfiguration,
            ),
            (LlmErrorKind::InvalidRequest, FailureClass::ProviderTransport),
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
        ] {
            assert_eq!(FailureClass::from_db_value(class.as_db_value()), Some(class));
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
}
