use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::persistence_service::{HitlDecision, PulsationStage, PulsationStatus};

use super::{
    CodeContextFacts, CollectionBudget, ContentSha256, ContractError, RelativeSourcePath, SchemaV1,
    SourceRevision,
};

pub const LEGACY_PIPELINE_VERSION: u8 = 1;
pub const CURRENT_PIPELINE_VERSION: u8 = 2;
pub const MAX_DESCRIPTION_CHARS: usize = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct VaguenessScore(u8);

impl VaguenessScore {
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for VaguenessScore {
    type Error = ContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value > 100 {
            Err(ContractError::Invalid("score fora de 0–100"))
        } else {
            Ok(Self(value))
        }
    }
}

impl From<VaguenessScore> for u8 {
    fn from(value: VaguenessScore) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct PreparationSettings {
    pub preparation_enabled: bool,
    pub vagueness_threshold: VaguenessScore,
    /// The final v2 default. M0 does not register a Learning worker or persist it.
    pub learning_enabled: bool,
}

impl Default for PreparationSettings {
    fn default() -> Self {
        Self {
            preparation_enabled: false,
            vagueness_threshold: VaguenessScore(40),
            learning_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreContribution {
    pub rule_id: String,
    pub weight: u8,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaguenessAssessment {
    pub schema_version: SchemaV1,
    pub ruleset_version: String,
    pub input_revision: u64,
    pub score: VaguenessScore,
    pub threshold: VaguenessScore,
    pub contributions: Vec<ScoreContribution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisePulsationDescriptionRequest {
    pub pulsation_id: String,
    pub expected_input_revision: u64,
    pub description: String,
}

impl RevisePulsationDescriptionRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.pulsation_id.trim().is_empty()
            || self.expected_input_revision == 0
            || self.description.trim().is_empty()
            || self.description.chars().count() > MAX_DESCRIPTION_CHARS
        {
            return Err(ContractError::Invalid("descrição/revisão da pulsação"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmDescriptionReviewRequest {
    pub pulsation_id: String,
    pub expected_input_revision: u64,
}

/// Compare with freshly loaded state *inside* the accepting transaction. This
/// value alone is neither a lock nor permission to write to the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionFence {
    pub project_id: String,
    /// Project maintenance need not manufacture a pulsation.
    pub pulsation_id: Option<String>,
    pub input_revision: Option<u64>,
    pub job_id: String,
    pub generation: u64,
    pub state_version: u64,
}

impl ExecutionFence {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.project_id.trim().is_empty()
            || self.job_id.trim().is_empty()
            || self.generation == 0
            || self.state_version == 0
            || self.pulsation_id.is_some() != self.input_revision.is_some()
            || self
                .pulsation_id
                .as_ref()
                .is_some_and(|id| id.trim().is_empty())
            || self.input_revision == Some(0)
        {
            return Err(ContractError::Invalid("identidade da execução"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobClass {
    Main,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "stage", rename_all = "snake_case")]
pub enum OrchestrationJobKind {
    RunStage(PulsationStage),
    PrepareContext,
    Learn,
    PublishArtifact,
    RefreshEmbeddings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationReason {
    UserCancel,
    Shutdown,
    Preempted,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseState {
    Ready,
    Publishing,
    NeedsReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPipelineState {
    pub project_id: String,
    pub active_pulsation_id: Option<String>,
    pub base_epoch: u64,
    pub base_state: BaseState,
    pub version: u64,
    pub blocking_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionReviewState {
    NotRequired,
    Pending,
    Confirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreSpecState {
    NotScheduled,
    Pending,
    Running,
    Ready,
    Skipped,
    Stale,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreSpecSkipReason {
    Disabled,
    SkippedNoPredecessor,
    AmbiguousRequest,
    NoMatches,
    BaseReleased,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationSnapshot {
    pub assessment: Option<VaguenessAssessment>,
    pub description_review: DescriptionReviewState,
    pub prespec_state: PreSpecState,
    pub context_artifact_id: Option<String>,
    pub skip_reason: Option<PreSpecSkipReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPackageV1 {
    pub schema_version: SchemaV1,
    pub pulsation_id: String,
    pub input_revision: u64,
    pub ruleset_version: String,
    pub observed_base_epoch: u64,
    pub budget: CollectionBudget,
    pub facts: CodeContextFacts,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecificationQuestion {
    pub id: String,
    pub question: String,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecificationAnswer {
    pub question_id: String,
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecificationQuestionSet {
    pub id: String,
    pub checkpoint_id: String,
    pub fence: ExecutionFence,
    pub conversation_id: String,
    pub questions: Vec<SpecificationQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitSpecificationAnswersRequest {
    pub pulsation_id: String,
    pub question_set_id: String,
    pub checkpoint_id: String,
    pub expected_input_revision: u64,
    pub expected_state_version: u64,
    pub answers: Vec<SpecificationAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyVersionedHitlDecisionRequest {
    pub pulsation_id: String,
    pub checkpoint_id: String,
    pub expected_state_version: u64,
    pub decision: HitlDecision,
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmBaseReconciliationRequest {
    pub project_id: String,
    pub expected_base_epoch: u64,
    pub expected_pipeline_state_version: u64,
    pub explanation: String,
    /// Explicit choice when several preserved, advanced pulsations need reconciliation.
    pub resume_pulsation_id: Option<String>,
    /// Binds the confirmation to the displayed artifact states and observed file hashes.
    pub expected_review_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePosition {
    pub pulsation_id: String,
    pub sequence: u64,
    pub blocking_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationReview {
    pub artifact_id: String,
    pub pulsation_id: String,
    pub relative_path: String,
    pub state: String,
    pub desired_sha256: Option<String>,
    pub observed_sha256: Option<String>,
    pub diagnostic: Option<String>,
    pub observation_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCoordinationSnapshot {
    pub pipeline: ProjectPipelineState,
    pub queue: Vec<QueuePosition>,
    pub owner_candidates: Vec<String>,
    pub publications: Vec<PublicationReview>,
    pub review_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageArtifactDraft {
    pub name: String,
    pub kind: String,
    pub relative_path: Option<RelativeSourcePath>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum StageOutcome {
    Completed {
        summary: String,
        notes: Vec<String>,
        artifacts: Vec<StageArtifactDraft>,
    },
    NeedsClarification {
        questions: Vec<SpecificationQuestion>,
    },
    Failed {
        code: String,
        message: String,
    },
}

impl StageOutcome {
    pub fn validate_for(&self, stage: PulsationStage) -> Result<(), ContractError> {
        match self {
            Self::Completed {
                summary, artifacts, ..
            } => {
                if summary.trim().is_empty()
                    || artifacts.iter().any(|item| {
                        item.name.trim().is_empty()
                            || item.kind.trim().is_empty()
                            || item.content.is_empty()
                    })
                {
                    return Err(ContractError::Invalid("resultado/artefato vazio"));
                }
            }
            Self::NeedsClarification { questions } => {
                if stage != PulsationStage::Specification {
                    return Err(ContractError::Invalid("perguntas fora de Specification"));
                }
                validate_questions(questions)?;
            }
            Self::Failed { code, message } => {
                if code.trim().is_empty() || message.trim().is_empty() {
                    return Err(ContractError::Invalid("falha sem diagnóstico"));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_questions(questions: &[SpecificationQuestion]) -> Result<(), ContractError> {
    let mut ids = HashSet::new();
    if questions.is_empty()
        || questions.iter().any(|item| {
            item.id.trim().is_empty()
                || item.question.trim().is_empty()
                || item.rationale.trim().is_empty()
                || !ids.insert(&item.id)
        })
    {
        return Err(ContractError::Invalid("perguntas vazias/duplicadas"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAcceptance {
    Candidate,
    Accepted,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    NotRequired,
    Pending,
    Applying,
    Applied,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactHandoff {
    pub artifact_id: String,
    pub producer: ExecutionFence,
    pub producer_stage: PulsationStage,
    pub output_position: u32,
    pub name: String,
    pub kind: String,
    pub content: String,
    pub content_sha256: ContentSha256,
    pub acceptance: ArtifactAcceptance,
    pub publication: PublicationState,
    pub source_revisions: Vec<SourceRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageHandoffV1 {
    pub schema_version: SchemaV1,
    pub recipient: ExecutionFence,
    pub stage: PulsationStage,
    pub assessment: Option<VaguenessAssessment>,
    pub context: Option<ContextPackageV1>,
    pub answers: Vec<SpecificationAnswer>,
    pub artifacts: Vec<ArtifactHandoff>,
    /// Last host-published bytes, including superseded drafts, for optimistic replacement only.
    /// These hashes do not promote the old content into normative context.
    #[serde(default)]
    pub publication_bases: Vec<SourceRevision>,
    pub revision_feedback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "sha256", rename_all = "snake_case")]
pub enum ExpectedFileContent {
    Absent,
    Present(ContentSha256),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPublicationIntent {
    pub schema_version: SchemaV1,
    pub artifact_id: String,
    pub producer: ExecutionFence,
    pub relative_path: RelativeSourcePath,
    pub expected: ExpectedFileContent,
    pub desired_sha256: ContentSha256,
    pub state: PublicationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningState {
    NotScheduled,
    Pending,
    Running,
    Published,
    Failed,
    Cancelled,
    DeferredBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationCoordinationSnapshot {
    pub pulsation_id: String,
    pub project_id: String,
    pub queue_sequence: u64,
    pub pipeline_version: u8,
    pub input_revision: u64,
    pub state_version: u64,
    pub stage: PulsationStage,
    pub status: PulsationStatus,
    pub blocking_reason: Option<String>,
    pub preparation: PreparationSnapshot,
    pub questions: Option<SpecificationQuestionSet>,
    pub learning: LearningState,
    pub artifact_ids: Vec<String>,
}
