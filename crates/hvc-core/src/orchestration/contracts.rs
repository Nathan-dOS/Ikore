use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::agent_output_language::{
    validate_agent_artifact, validate_english_prose, validate_english_text,
};
use crate::persistence_service::{HitlDecision, PulsationStage, PulsationStatus};

use super::{
    CodeContextFacts, CollectionBudget, ContentSha256, ContractError, RelativeSourcePath, SchemaV1,
    SourceRevision,
};

pub const LEGACY_PIPELINE_VERSION: u8 = super::PipelineVersion::V1.as_u8();
pub const CURRENT_PIPELINE_VERSION: u8 = super::PipelineVersion::FREE_FORM.as_u8();
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
    pub learning_refinement_mode: super::LearningRefinementMode,
    pub brownfield_llm_budget: u8,
    pub brownfield_expansion_radius: u8,
}

impl Default for PreparationSettings {
    fn default() -> Self {
        Self {
            preparation_enabled: false,
            vagueness_threshold: VaguenessScore(40),
            learning_enabled: true,
            learning_refinement_mode: super::LearningRefinementMode::Observe,
            brownfield_llm_budget: 1,
            brownfield_expansion_radius: 1,
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

/// Discrete milestones for background embedding reconciliation, pushed to the UI over
/// `AppEventBus`. `remaining` is a countdown of pending/running `index_embedding` jobs for the
/// workspace, not a fixed total: `reconcile_index_batch` paginates, so the queue can grow between
/// passes and a fixed "X/Y" would lie mid-run.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexingEvent {
    Progress {
        workspace_path: String,
        remaining: usize,
    },
    Paused {
        workspace_path: String,
        remaining: usize,
    },
    Idle {
        workspace_path: String,
    },
    Failed {
        workspace_path: String,
        error: String,
    },
}

impl IndexingEvent {
    pub fn workspace_path(&self) -> &str {
        match self {
            Self::Progress { workspace_path, .. }
            | Self::Paused { workspace_path, .. }
            | Self::Idle { workspace_path }
            | Self::Failed { workspace_path, .. } => workspace_path,
        }
    }
}

pub trait IndexingEventSink: Send + Sync {
    fn emit(&self, event: &IndexingEvent);
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
pub struct ClarificationExchange {
    pub question_set_id: String,
    pub questions: Vec<SpecificationQuestion>,
    pub answers: Vec<ClarificationAnswerV1>,
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

/// A developer's answer to a clarification question (ADR-022 clarification
/// loop). `text_answer` keeps the full prose reply, including any `FILES_*:`
/// tag lines the Companion or the developer wrote; `planned_paths`/
/// `modified_paths` are the host-parsed anchors extracted from those tags
/// (see `orchestration::clarification::parse_clarification_tags`), populated
/// once at submission time rather than recomputed on every read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClarificationAnswerV1 {
    pub question_id: String,
    #[serde(alias = "answer")]
    pub text_answer: String,
    #[serde(default)]
    pub planned_paths: Vec<String>,
    #[serde(default)]
    pub modified_paths: Vec<String>,
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
    pub answers: Vec<ClarificationAnswerV1>,
}

/// One task's manual path correction, resolving a preventive path-confirmation
/// checkpoint (ADR-022 clarification loop, P5/A5.2) -- the same semantics as
/// `feature_forge::TaskSelection::manual_path_hint` at dispatch time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualPathHint {
    pub task_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvePathConfirmationRequest {
    pub pulsation_id: String,
    pub checkpoint_id: String,
    pub expected_state_version: u64,
    #[serde(default)]
    pub manual_path_hints: Vec<ManualPathHint>,
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

/// What the cleanup of a failed pulsation does with one workspace path it
/// published. Only a file the pulsation itself created, still holding bytes
/// the pulsation wrote, is ever removed; everything else is kept and reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupDisposition {
    /// Created by the pulsation and unchanged since: removed.
    Remove,
    /// The file is already gone; the record is closed without touching disk.
    AlreadyAbsent,
    /// The pulsation overwrote a file that existed before it. The previous
    /// content is not stored, so the file is left for a human decision.
    KeepPreexisting,
    /// Created by the pulsation but edited afterwards: kept.
    KeepEdited,
    /// The path could not be observed (protected or unreadable): kept.
    KeepUnreadable,
}

impl CleanupDisposition {
    pub fn removes(self) -> bool {
        matches!(self, Self::Remove)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedPulsationCleanupEntry {
    pub relative_path: String,
    pub disposition: CleanupDisposition,
    /// Operator-facing reason for a kept path (e.g. the observation error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Dry run of [`CleanupFailedPulsationFilesRequest`]: what would be removed and
/// what is deliberately kept. `plan_token` binds the confirmation to exactly
/// this list and the observed file hashes behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedPulsationCleanupPlan {
    pub pulsation_id: String,
    pub project_id: String,
    pub entries: Vec<FailedPulsationCleanupEntry>,
    /// Disposable staging directories under the app data dir
    /// (`stage_candidates/<execution>`), shown by directory name.
    pub candidate_dirs: Vec<String>,
    pub plan_token: String,
}

impl FailedPulsationCleanupPlan {
    pub fn removable_files(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.disposition.removes())
            .count()
    }

    pub fn kept_files(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                !matches!(
                    entry.disposition,
                    CleanupDisposition::Remove | CleanupDisposition::AlreadyAbsent
                )
            })
            .count()
    }

    /// True when confirming would change nothing on disk or in the database.
    pub fn is_noop(&self) -> bool {
        self.candidate_dirs.is_empty()
            && !self.entries.iter().any(|entry| {
                matches!(
                    entry.disposition,
                    CleanupDisposition::Remove | CleanupDisposition::AlreadyAbsent
                )
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CleanupFailedPulsationFilesRequest {
    pub pulsation_id: String,
    /// The `plan_token` of the preview the operator confirmed.
    pub expected_plan_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedPulsationCleanupReport {
    pub pulsation_id: String,
    pub removed_files: Vec<String>,
    /// Parent directories that became empty and were pruned up to the workspace root.
    pub removed_directories: usize,
    pub removed_candidate_dirs: usize,
    pub kept_files: usize,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentSourceDraft {
    pub relative_path: RelativeSourcePath,
    pub content_sha256: ContentSha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentProposalDraft {
    pub component_id: String,
    pub title: String,
    pub summary: String,
    pub confidence: u8,
    pub source_files: Vec<ComponentSourceDraft>,
    #[serde(default)]
    pub related_components: Vec<String>,
}

impl ComponentProposalDraft {
    pub fn validate(&self) -> Result<(), ContractError> {
        let valid_id = |value: &str| {
            !value.is_empty()
                && value.len() <= 80
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && value
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                && value
                    .bytes()
                    .last()
                    .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                && value.split('-').all(|part| !part.is_empty())
        };
        let mut sources = HashSet::new();
        let mut related = HashSet::new();
        if !valid_id(&self.component_id)
            || self.title.trim().is_empty()
            || self.title.chars().count() > 160
            || self.summary.trim().is_empty()
            || self.summary.chars().count() > 4_800
            || self.confidence > 100
            || self.source_files.is_empty()
            || self.source_files.len() > 12
            || self
                .source_files
                .iter()
                .any(|source| !sources.insert(&source.relative_path))
            || self.related_components.len() > 12
            || self.related_components.iter().any(|component| {
                component == &self.component_id
                    || !valid_id(component)
                    || !related.insert(component)
            })
        {
            return Err(ContractError::Invalid("proposta de componente"));
        }
        validate_english_text("component proposal title", &self.title)
            .and_then(|_| validate_english_prose("component proposal summary", &self.summary))
            .map_err(|_| ContractError::Invalid("component proposal must be English"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentProposalStatus {
    Proposed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentProposalArtifactV1 {
    pub schema_version: SchemaV1,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub status: ComponentProposalStatus,
    pub proposal: ComponentProposalDraft,
}

impl ComponentProposalArtifactV1 {
    pub fn proposed(proposal: ComponentProposalDraft) -> Result<Self, ContractError> {
        proposal.validate()?;
        Ok(Self {
            schema_version: SchemaV1,
            artifact_type: "component_proposal".into(),
            status: ComponentProposalStatus::Proposed,
            proposal,
        })
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.artifact_type != "component_proposal" {
            return Err(ContractError::Invalid("tipo da proposta de componente"));
        }
        self.proposal.validate()
    }
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

/// Which artifacts the English guard judges at acceptance.
///
/// A legacy stage accepts prose the model composed, so every artifact is
/// judged whole. A composed stage (ADR-022) judges each model-authored field
/// before rendering (SC-008) and then renders documents that quote the
/// developer's own task titles and criteria verbatim; judging those documents
/// again would reject a correct English report for the Portuguese scope it
/// was required to copy. Only the composed coordinator selects `HostRendered`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtifactLanguageGuard {
    #[default]
    ModelAuthored,
    HostRendered,
}

impl StageOutcome {
    pub fn validate_for(&self, stage: PulsationStage) -> Result<(), ContractError> {
        self.validate_with(stage, ArtifactLanguageGuard::ModelAuthored)
    }

    pub fn validate_with(
        &self,
        stage: PulsationStage,
        guard: ArtifactLanguageGuard,
    ) -> Result<(), ContractError> {
        match self {
            Self::Completed {
                summary, artifacts, ..
            } => {
                let component_proposals = artifacts
                    .iter()
                    .filter(|artifact| artifact.kind == "component_proposal")
                    .count();
                if summary.trim().is_empty()
                    || component_proposals > 2
                    || artifacts.iter().any(|item| {
                        item.name.trim().is_empty()
                            || item.kind.trim().is_empty()
                            || item.content.is_empty()
                    })
                {
                    return Err(ContractError::Invalid("resultado/artefato vazio"));
                }
                for artifact in artifacts
                    .iter()
                    .filter(|artifact| artifact.kind == "component_proposal")
                {
                    if stage != PulsationStage::Specification
                        || artifact.relative_path.is_some()
                        || serde_json::from_str::<ComponentProposalArtifactV1>(&artifact.content)
                            .map_err(|_| ContractError::Invalid("artifact component_proposal"))?
                            .validate()
                            .is_err()
                    {
                        return Err(ContractError::Invalid("artifact component_proposal"));
                    }
                }
                if guard == ArtifactLanguageGuard::ModelAuthored {
                    for artifact in artifacts {
                        validate_agent_artifact(
                            &artifact.name,
                            artifact
                                .relative_path
                                .as_ref()
                                .map(RelativeSourcePath::as_str),
                            &artifact.content,
                        )
                        .map_err(|_| ContractError::Invalid("agent artifact must be English"))?;
                    }
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
        || questions.len() > 8
        || questions.iter().any(|item| {
            item.id.trim().is_empty()
                || item.id.chars().count() > 80
                || item.question.chars().count() > 2000
                || item.rationale.chars().count() > 1000
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_manifest: Option<crate::feature_forge::BatchManifest>,
    pub schema_version: SchemaV1,
    pub recipient: ExecutionFence,
    pub stage: PulsationStage,
    pub assessment: Option<VaguenessAssessment>,
    pub context: Option<ContextPackageV1>,
    pub answers: Vec<ClarificationAnswerV1>,
    #[serde(default)]
    pub clarifications: Vec<ClarificationExchange>,
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
    Unavailable,
    NotScheduled,
    Pending,
    Running,
    Published,
    Failed,
    Cancelled,
    DeferredBudget,
    Preempted,
    Disabled,
    Publishing,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationCoordinationSnapshot {
    pub description_editable: bool,
    pub context_disposition: Option<String>,
    pub diagnostic: Option<String>,
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
    pub learning_run: Option<super::LearningRunSnapshot>,
    pub artifact_ids: Vec<String>,
    /// The latest composed stage execution (pipeline v4) with its units,
    /// attempts, checks and remaining budget; `None` for v1-v3 pulsations
    /// or before the first admission (ADR-022).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_execution: Option<super::composition::StageExecutionSnapshot>,
}
