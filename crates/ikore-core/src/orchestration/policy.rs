use std::collections::HashSet;

use crate::persistence_service::{JobStatus, PulsationStage};

use super::{
    ArtifactAcceptance, ArtifactLanguageGuard, CancellationReason, ContentSha256, ContractError,
    ExecutionFence, ExpectedFileContent, PublicationState, SpecificationQuestionSet,
    StageHandoffV1, StageOutcome, SubmitSpecificationAnswersRequest,
};

/// M1 must perform this check within the same transaction that accepts a result.
pub fn validate_result_acceptance(
    submitted: &ExecutionFence,
    current: &ExecutionFence,
    status: JobStatus,
    cancellation: Option<CancellationReason>,
) -> Result<(), ContractError> {
    submitted.validate()?;
    current.validate()?;
    if submitted != current {
        return Err(ContractError::StaleExecution);
    }
    if status != JobStatus::Running || cancellation.is_some() {
        return Err(ContractError::ExecutionClosed);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageDisposition {
    Advance(PulsationStage),
    AwaitArtifactReview,
    AwaitClarification,
    Fail,
}

/// A model cannot request skipping the host's existing mandatory checkpoints.
/// Advance is an intent, not a claim: the scheduler still enforces the base gate.
pub fn stage_disposition(
    stage: PulsationStage,
    outcome: &StageOutcome,
) -> Result<StageDisposition, ContractError> {
    stage_disposition_with(stage, outcome, ArtifactLanguageGuard::ModelAuthored)
}

/// Same disposition with an explicit English guard (see
/// [`ArtifactLanguageGuard`]); the composed stage passes `HostRendered`.
pub fn stage_disposition_with(
    stage: PulsationStage,
    outcome: &StageOutcome,
    guard: ArtifactLanguageGuard,
) -> Result<StageDisposition, ContractError> {
    outcome.validate_with(stage, guard)?;
    Ok(match outcome {
        StageOutcome::Failed { .. } => StageDisposition::Fail,
        StageOutcome::NeedsClarification { .. } => StageDisposition::AwaitClarification,
        StageOutcome::Completed { .. } if stage.requires_checkpoint() => {
            StageDisposition::AwaitArtifactReview
        }
        StageOutcome::Completed { .. } => StageDisposition::Advance(
            stage
                .next()
                .ok_or(ContractError::Invalid("stage without a successor"))?,
        ),
    })
}

pub fn revision_target(stage: PulsationStage) -> Option<PulsationStage> {
    match stage {
        PulsationStage::Specification | PulsationStage::Contract => Some(stage),
        PulsationStage::Test | PulsationStage::Integrator => Some(PulsationStage::Code),
        PulsationStage::Intake | PulsationStage::Code => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationReplayDecision {
    Apply,
    AcknowledgeAlreadyApplied,
    Conflict,
}

/// Classifies a crash window without performing I/O. The publisher must read
/// observed content under its write coordinator and recheck cancellation/fencing.
pub fn publication_replay_decision(
    expected: &ExpectedFileContent,
    desired: &ContentSha256,
    observed: Option<&ContentSha256>,
) -> PublicationReplayDecision {
    if observed == Some(desired) {
        return PublicationReplayDecision::AcknowledgeAlreadyApplied;
    }
    match (expected, observed) {
        (ExpectedFileContent::Absent, None) => PublicationReplayDecision::Apply,
        (ExpectedFileContent::Present(expected), Some(observed)) if expected == observed => {
            PublicationReplayDecision::Apply
        }
        _ => PublicationReplayDecision::Conflict,
    }
}

pub fn validate_specification_answers(
    request: &SubmitSpecificationAnswersRequest,
    pending: &SpecificationQuestionSet,
) -> Result<(), ContractError> {
    pending.fence.validate()?;
    super::contracts::validate_questions(&pending.questions)?;
    if pending.fence.pulsation_id.as_deref() != Some(request.pulsation_id.as_str())
        || request.question_set_id != pending.id
        || request.checkpoint_id != pending.checkpoint_id
        || Some(request.expected_input_revision) != pending.fence.input_revision
        || request.expected_state_version != pending.fence.state_version
    {
        return Err(ContractError::StaleExecution);
    }
    let expected: HashSet<_> = pending
        .questions
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    let mut received = HashSet::new();
    if request.answers.len() != expected.len()
        || request.answers.iter().any(|answer| {
            answer.text_answer.trim().is_empty()
                || answer.text_answer.chars().count() > 16_000
                || !expected.contains(answer.question_id.as_str())
                || !received.insert(answer.question_id.as_str())
        })
    {
        return Err(ContractError::Invalid(
            "respostas incompletas/duplicadas/desconhecidas",
        ));
    }
    Ok(())
}

impl StageHandoffV1 {
    pub fn normative_chars(&self) -> usize {
        self.batch_manifest.as_ref().map_or(0, |manifest| {
            crate::feature_forge::batch_budget::batch_prompt_projection(manifest)
                .and_then(|projection| serde_json::to_string(&projection))
                .map_or(usize::MAX / 2, |text| text.chars().count())
        }) + self
            .artifacts
            .iter()
            .map(|a| a.content.chars().count())
            .sum::<usize>()
            + self
                .revision_feedback
                .as_ref()
                .map_or(0, |s| s.chars().count())
            + if self.clarifications.is_empty() {
                0
            } else {
                serde_json::to_string(&self.clarifications)
                    .map_or(usize::MAX / 2, |s| s.chars().count())
            }
            + self
                .answers
                .iter()
                .map(|a| a.text_answer.chars().count())
                .sum::<usize>()
    }
    /// Validates the envelope, not the external filesystem. M3 still revalidates
    /// selected sources; M1 loads artifact acceptance from authoritative rows.
    pub fn validate_for(
        &self,
        recipient: &ExecutionFence,
        artifact_chars_budget: usize,
    ) -> Result<(), ContractError> {
        let mut paths = HashSet::new();
        for base in &self.publication_bases {
            if base.project_id != recipient.project_id || !paths.insert(&base.relative_path) {
                return Err(ContractError::Invalid(
                    "duplicate publication bases or from another project",
                ));
            }
        }
        recipient.validate()?;
        if self.normative_chars() > artifact_chars_budget {
            return Err(ContractError::BudgetExceeded);
        }
        if &self.recipient != recipient || recipient.pulsation_id.is_none() {
            return Err(ContractError::StaleExecution);
        }
        if self
            .assessment
            .as_ref()
            .is_some_and(|assessment| Some(assessment.input_revision) != recipient.input_revision)
        {
            return Err(ContractError::StaleExecution);
        }
        if let Some(context) = &self.context {
            context.budget.validate()?;
            context.facts.validate()?;
            if Some(context.pulsation_id.as_str()) != recipient.pulsation_id.as_deref()
                || Some(context.input_revision) != recipient.input_revision
                || context.facts.project_id != recipient.project_id
            {
                return Err(ContractError::StaleExecution);
            }
            if self.normative_chars().saturating_add(
                context
                    .prompt_projection_chars()
                    .map_err(|_| ContractError::Invalid("contexto"))?,
            ) > artifact_chars_budget
            {
                return Err(ContractError::BudgetExceeded);
            }
        }
        let mut artifact_ids = HashSet::new();
        let mut producer_positions = HashSet::new();
        let mut used_chars = 0usize;
        used_chars += self
            .answers
            .iter()
            .map(|a| a.text_answer.chars().count())
            .sum::<usize>();
        used_chars += self
            .revision_feedback
            .as_ref()
            .map_or(0, |s| s.chars().count());
        if !self.clarifications.is_empty() {
            used_chars += serde_json::to_string(&self.clarifications)
                .map_err(|_| ContractError::Invalid("clarification history"))?
                .chars()
                .count();
        }
        if used_chars > artifact_chars_budget {
            return Err(ContractError::BudgetExceeded);
        }
        for artifact in &self.artifacts {
            artifact.producer.validate()?;
            if artifact.producer.project_id != recipient.project_id
                || artifact.producer.pulsation_id != recipient.pulsation_id
                || artifact.producer.input_revision != recipient.input_revision
                || artifact
                    .source_revisions
                    .iter()
                    .any(|source| source.project_id != recipient.project_id)
            {
                return Err(ContractError::StaleExecution);
            }
            if artifact.acceptance != ArtifactAcceptance::Accepted
                || !matches!(
                    artifact.publication,
                    PublicationState::Applied | PublicationState::NotRequired
                )
                || ContentSha256::of(artifact.content.as_bytes()) != artifact.content_sha256
                || artifact.artifact_id.trim().is_empty()
                || !artifact_ids.insert(&artifact.artifact_id)
                || !producer_positions.insert((
                    &artifact.producer.job_id,
                    artifact.producer.generation,
                    artifact.output_position,
                ))
            {
                return Err(ContractError::Invalid(
                    "artifact not accepted, published or intact",
                ));
            }
            let mut applicable_stage = Some(artifact.producer_stage);
            while applicable_stage.is_some_and(|stage| stage != self.stage) {
                applicable_stage = applicable_stage.and_then(PulsationStage::next);
            }
            if applicable_stage.is_none() {
                return Err(ContractError::Invalid(
                    "artifact from a stage after the recipient",
                ));
            }
            used_chars = used_chars
                .checked_add(artifact.content.chars().count())
                .ok_or(ContractError::BudgetExceeded)?;
            if used_chars > artifact_chars_budget {
                return Err(ContractError::BudgetExceeded);
            }
        }
        Ok(())
    }
}
