use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::orchestration::{
    BaseState, ConfirmBaseReconciliationRequest, ProjectCoordinationSnapshot,
};
use hvc_core::persistence_service::{
    Artifact, CancelPulsationJobsRequest, CreatePulsationRequest,
    GetPulsationRuntimeSnapshotRequest, ListPulsationArtifactsRequest, Pulsation,
    PulsationRuntimeSnapshot, PulsationStage, PulsationStatus,
};

use crate::hitl::HitlReviewState;
use crate::workspace::WorkspaceViewMode;
use hvc_core::orchestration::{
    ConfirmDescriptionReviewRequest, PulsationCoordinationSnapshot,
    RevisePulsationDescriptionRequest, SubmitSpecificationAnswersRequest,
};

pub(crate) type PulsationsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

pub(crate) trait PulsationsBackend: Send + Sync {
    fn action(&self, action: PulsationAction) -> PulsationsFuture<()>;
    fn artifact_content(&self, pulse: String, artifact: String) -> PulsationsFuture<String>;
    fn runtime_snapshot(&self, project_id: String) -> PulsationsFuture<PulsationRuntimeSnapshot>;
    fn artifacts(&self, pulsation_id: String) -> PulsationsFuture<Vec<Artifact>>;
    fn create(&self, project_id: String, feature_name: String) -> PulsationsFuture<Pulsation>;
    fn cancel_jobs(&self, pulsation_id: String) -> PulsationsFuture<usize>;
    fn reconcile(&self, request: ConfirmBaseReconciliationRequest) -> PulsationsFuture<()>;
}

#[derive(Debug, Clone)]
pub(crate) enum PulsationAction {
    Revise(RevisePulsationDescriptionRequest),
    Confirm(ConfirmDescriptionReviewRequest),
    Answer(SubmitSpecificationAnswersRequest),
    RetryLearning(hvc_core::orchestration::LearningActionRequest),
    DismissLearning(hvc_core::orchestration::LearningActionRequest),
}

pub(crate) struct CorePulsationsBackend {
    core: Arc<AppCore>,
}

impl CorePulsationsBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }

    fn worker_error(operation: &str, error: tokio::task::JoinError) -> String {
        format!("worker de {operation} de pulsação interrompido: {error}")
    }
}

impl PulsationsBackend for CorePulsationsBackend {
    fn action(&self, action: PulsationAction) -> PulsationsFuture<()> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || match action {
                    PulsationAction::RetryLearning(request) => {
                        core.persistence().retry_learning(request)
                    }
                    PulsationAction::DismissLearning(request) => {
                        core.persistence().dismiss_learning(request)
                    }
                    PulsationAction::Revise(request) => core
                        .persistence()
                        .revise_pulsation_description(request)
                        .map(|_| ()),
                    PulsationAction::Confirm(request) => {
                        core.persistence().confirm_description_review(request)
                    }
                    PulsationAction::Answer(request) => core
                        .persistence()
                        .submit_specification_answers(request)
                        .map(|_| ()),
                })
                .await
                .map_err(|e| Self::worker_error("ação", e))?
                .map_err(|e| e.to_string())
        })
    }
    fn artifact_content(&self, pulse: String, artifact: String) -> PulsationsFuture<String> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().read_artifact_content(&pulse, &artifact))
                .await
                .map_err(|e| Self::worker_error("conteúdo", e))?
                .map_err(|e| e.to_string())
        })
    }
    fn reconcile(&self, request: ConfirmBaseReconciliationRequest) -> PulsationsFuture<()> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .confirm_base_reconciliation(request)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| Self::worker_error("reconciliação", e))?
        })
    }

    fn runtime_snapshot(&self, project_id: String) -> PulsationsFuture<PulsationRuntimeSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .get_pulsation_runtime_snapshot(GetPulsationRuntimeSnapshotRequest {
                            project_id,
                        })
                        .map(|response| response.snapshot)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("snapshot", error))?
        })
    }

    fn artifacts(&self, pulsation_id: String) -> PulsationsFuture<Vec<Artifact>> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .list_pulsation_artifacts(ListPulsationArtifactsRequest { pulsation_id })
                        .map(|response| response.artifacts)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("artefatos", error))?
        })
    }

    fn create(&self, project_id: String, feature_name: String) -> PulsationsFuture<Pulsation> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .create_pulsation(CreatePulsationRequest {
                            project_id,
                            feature_name,
                        })
                        .map(|response| response.pulsation)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("criação", error))?
        })
    }

    fn cancel_jobs(&self, pulsation_id: String) -> PulsationsFuture<usize> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .cancel_pulsation_jobs(CancelPulsationJobsRequest { pulsation_id })
                        .map(|response| response.cancelled_jobs)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("cancelamento", error))?
        })
    }
}

#[derive(Default)]
pub struct PulsationsModel {
    pub details: Vec<PulsationCoordinationSnapshot>,
    pub action_busy: bool,
    pub artifact_preview: Option<(String, String)>,
    pub coordination: Option<ProjectCoordinationSnapshot>,
    pub reconciliation_busy: bool,
    pub hitl_checkpoints: Vec<hvc_core::persistence_service::HitlCheckpoint>,
    pub project_id: Option<SharedString>,
    pub pulsations: Vec<Pulsation>,
    pub active_pulsation_id: Option<SharedString>,
    pub artifacts: Vec<Artifact>,
    pub running_jobs: usize,
    pub awaiting_hitl: usize,
    pub runtime_loading: bool,
    pub artifacts_loading: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub workspace_view_override: Option<WorkspaceViewMode>,
    pub(crate) hitl: HitlReviewState,
}

impl PulsationsModel {
    pub(crate) fn detail(&self) -> Option<&PulsationCoordinationSnapshot> {
        let active = self.active()?;
        self.details.iter().find(|d| d.pulsation_id == active.id)
    }
    pub(crate) fn reset(&mut self, project_id: Option<String>) {
        *self = Self {
            project_id: project_id.map(Into::into),
            ..Self::default()
        };
    }

    pub(crate) fn active(&self) -> Option<&Pulsation> {
        let active_id = self.active_pulsation_id.as_ref().map(AsRef::as_ref);
        self.pulsations
            .iter()
            .find(|pulsation| Some(pulsation.id.as_str()) == active_id)
            .or_else(|| self.pulsations.first())
    }

    pub(crate) fn active_id(&self) -> Option<String> {
        self.active().map(|pulsation| pulsation.id.clone())
    }

    pub(crate) fn apply_snapshot(
        &mut self,
        project_id: String,
        snapshot: PulsationRuntimeSnapshot,
        preferred_id: Option<&str>,
    ) -> Option<String> {
        let current_id = self.active_pulsation_id.as_ref().map(AsRef::as_ref);
        let next_id = preferred_id
            .filter(|id| snapshot.pulsations.iter().any(|item| item.id == *id))
            .or_else(|| {
                current_id.filter(|id| snapshot.pulsations.iter().any(|item| item.id == *id))
            })
            .map(str::to_owned)
            .or_else(|| snapshot.pulsations.first().map(|item| item.id.clone()));
        self.project_id = Some(project_id.into());
        self.pulsations = snapshot.pulsations;
        self.details = snapshot.details;
        self.hitl_checkpoints = snapshot.hitl_checkpoints;
        self.awaiting_hitl = snapshot.hitl_waiting_count;
        self.running_jobs = snapshot.main_agents_count + snapshot.background_agents_count;
        self.coordination = snapshot.coordination;
        self.active_pulsation_id = next_id.clone().map(Into::into);
        self.runtime_loading = false;
        self.error = None;
        let checkpoint = self
            .active()
            .filter(|pulsation| {
                pulsation.status == PulsationStatus::Checkpoint
                    && self
                        .hitl_checkpoints
                        .iter()
                        .any(|c| c.pulsation_id == pulsation.id)
            })
            .cloned();
        self.hitl.load_checkpoint(checkpoint.as_ref());
        self.hitl.expected_checkpoint = self
            .hitl_checkpoints
            .iter()
            .find(|c| Some(c.pulsation_id.as_str()) == self.hitl.pulsation_id.as_deref())
            .cloned();
        next_id
    }

    pub(crate) fn select(&mut self, pulsation_id: &str) -> bool {
        if !self
            .pulsations
            .iter()
            .any(|pulsation| pulsation.id == pulsation_id)
        {
            return false;
        }
        self.active_pulsation_id = Some(pulsation_id.to_owned().into());
        self.artifacts.clear();
        self.artifact_preview = None;
        self.notice = None;
        let checkpoint = self
            .active()
            .filter(|pulsation| {
                pulsation.status == PulsationStatus::Checkpoint
                    && self
                        .hitl_checkpoints
                        .iter()
                        .any(|c| c.pulsation_id == pulsation.id)
            })
            .cloned();
        self.hitl.load_checkpoint(checkpoint.as_ref());
        self.hitl.expected_checkpoint = self
            .hitl_checkpoints
            .iter()
            .find(|c| Some(c.pulsation_id.as_str()) == self.hitl.pulsation_id.as_deref())
            .cloned();
        true
    }

    pub(crate) fn begin_reconciliation(
        &mut self,
        explanation: String,
    ) -> Option<ConfirmBaseReconciliationRequest> {
        if self.reconciliation_busy || explanation.trim().is_empty() {
            return None;
        }
        let snapshot = self.coordination.as_ref()?;
        if snapshot.pipeline.base_state != BaseState::NeedsReview {
            return None;
        }
        let resume = if snapshot.owner_candidates.is_empty() {
            None
        } else {
            let selected = self.active_id()?;
            if !snapshot.owner_candidates.contains(&selected) {
                return None;
            }
            Some(selected)
        };
        let request = ConfirmBaseReconciliationRequest {
            project_id: snapshot.pipeline.project_id.clone(),
            expected_base_epoch: snapshot.pipeline.base_epoch,
            expected_pipeline_state_version: snapshot.pipeline.version,
            explanation,
            resume_pulsation_id: resume,
            expected_review_token: snapshot.review_token.clone(),
        };
        self.reconciliation_busy = true;
        Some(request)
    }

    pub(crate) fn finish_artifacts(
        &mut self,
        pulsation_id: &str,
        result: Result<Vec<Artifact>, String>,
    ) {
        if self.active_id().as_deref() != Some(pulsation_id) {
            return;
        }
        self.artifacts_loading = false;
        match result {
            Ok(artifacts) => {
                self.artifacts = artifacts;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn derived_workspace_mode(&self) -> WorkspaceViewMode {
        self.workspace_view_override.unwrap_or_else(|| {
            if self.active().is_some_and(|pulsation| {
                pulsation.status == PulsationStatus::Checkpoint
                    && self
                        .hitl_checkpoints
                        .iter()
                        .any(|c| c.pulsation_id == pulsation.id)
            }) {
                WorkspaceViewMode::Hitl
            } else {
                WorkspaceViewMode::Editor
            }
        })
    }

    pub(crate) fn set_workspace_override(&mut self, mode: WorkspaceViewMode) {
        self.workspace_view_override = Some(mode);
    }

    pub(crate) fn clear_workspace_override(&mut self) {
        self.workspace_view_override = None;
    }
}

pub(crate) const PIPELINE_STAGES: [PulsationStage; 6] = [
    PulsationStage::Intake,
    PulsationStage::Specification,
    PulsationStage::Contract,
    PulsationStage::Code,
    PulsationStage::Test,
    PulsationStage::Integrator,
];

pub(crate) fn stage_label(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Intake => "Intake",
        PulsationStage::Specification => "SPECIFICATION",
        PulsationStage::Contract => "Contract",
        PulsationStage::Code => "Code",
        PulsationStage::Test => "Test",
        PulsationStage::Integrator => "Integrator",
    }
}

pub(crate) fn status_label(status: PulsationStatus, terminal_reason: Option<&str>) -> &'static str {
    if status == PulsationStatus::Failed {
        match terminal_reason {
            Some("user_cancel") => return "Cancelada",
            Some("publication_conflict") => return "Conflito de publicação",
            _ => {}
        }
    }
    match status {
        PulsationStatus::Idle => "Idle",
        PulsationStatus::Planning => "Planning",
        PulsationStatus::Running => "Running",
        PulsationStatus::Checkpoint => "Checkpoint",
        PulsationStatus::Done => "Done",
        PulsationStatus::Failed => "Failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulsation(id: &str, status: PulsationStatus) -> Pulsation {
        Pulsation {
            terminal_reason: None,
            id: id.into(),
            project_id: "project-1".into(),
            feature_name: "Login".into(),
            stage: PulsationStage::Code,
            status,
            created_at: "2026-08-22T00:00:00Z".into(),
            updated_at: "2026-08-22T00:01:00Z".into(),
        }
    }

    fn artifact(id: &str, name: &str) -> Artifact {
        Artifact {
            id: id.into(),
            pulsation_id: "pulse-1".into(),
            name: name.into(),
            kind: "markdown".into(),
            relative_path: Some("Features/Login.md".into()),
            content_sha256: Some("abc".into()),
            created_at: "2026-08-22T00:01:00Z".into(),
            updated_at: "2026-08-22T00:01:00Z".into(),
        }
    }

    #[test]
    fn hydrates_checkpoint_counters_and_artifacts() {
        let mut model = PulsationsModel::default();
        let active = model.apply_snapshot(
            "project-1".into(),
            PulsationRuntimeSnapshot {
                details: Vec::new(),
                coordination: None,
                main_agents_count: 0,
                hitl_checkpoints: vec![hvc_core::persistence_service::HitlCheckpoint {
                    pulsation_id: "pulse-1".into(),
                    checkpoint_id: "cp-1".into(),
                    state_version: 1,
                }],
                pulsations: vec![pulsation("pulse-1", PulsationStatus::Checkpoint)],
                hitl_waiting_count: 1,
                background_agents_count: 2,
            },
            Some("pulse-1"),
        );
        model.artifacts_loading = true;
        model.finish_artifacts("pulse-1", Ok(vec![artifact("artifact-1", "Login plan")]));

        assert_eq!(active.as_deref(), Some("pulse-1"));
        assert_eq!(model.awaiting_hitl, 1);
        assert_eq!(model.running_jobs, 2);
        assert_eq!(model.derived_workspace_mode(), WorkspaceViewMode::Hitl);
        assert_eq!(model.artifacts[0].name, "Login plan");
        assert_eq!(model.hitl.pulsation_id.as_deref(), Some("pulse-1"));
    }

    #[test]
    fn refreshes_artifacts_for_selected_pulsation() {
        let mut model = PulsationsModel::default();
        model.apply_snapshot(
            "project-1".into(),
            PulsationRuntimeSnapshot {
                details: Vec::new(),
                coordination: None,
                main_agents_count: 0,
                hitl_checkpoints: Vec::new(),
                pulsations: vec![pulsation("pulse-1", PulsationStatus::Running)],
                hitl_waiting_count: 0,
                background_agents_count: 1,
            },
            None,
        );
        model.finish_artifacts("pulse-1", Ok(vec![artifact("artifact-1", "Plan")]));
        model.finish_artifacts(
            "pulse-1",
            Ok(vec![artifact("artifact-2", "Generated code")]),
        );
        model.finish_artifacts("stale-pulse", Ok(vec![artifact("stale", "Stale")]));

        assert_eq!(model.artifacts.len(), 1);
        assert_eq!(model.artifacts[0].name, "Generated code");
    }

    fn blocked_model(candidates: Vec<String>) -> PulsationsModel {
        PulsationsModel {
            pulsations: vec![
                pulsation("pulse-1", PulsationStatus::Checkpoint),
                pulsation("pulse-2", PulsationStatus::Checkpoint),
            ],
            active_pulsation_id: Some("pulse-1".into()),
            coordination: Some(ProjectCoordinationSnapshot {
                pipeline: hvc_core::orchestration::ProjectPipelineState {
                    project_id: "project-1".into(),
                    active_pulsation_id: None,
                    base_epoch: 2,
                    base_state: BaseState::NeedsReview,
                    version: 7,
                    blocking_reason: Some("multiple_legacy_owners".into()),
                },
                queue: vec![],
                owner_candidates: candidates,
                publications: vec![],
                review_token: "reviewed-files".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reconciliation_requires_explanation_and_an_explicit_eligible_selection() {
        let mut model = blocked_model(vec!["pulse-2".into()]);
        assert!(model.begin_reconciliation(" ".into()).is_none());
        assert!(model.begin_reconciliation("review".into()).is_none());
        model.select("pulse-2");
        let request = model.begin_reconciliation("review".into()).unwrap();
        assert_eq!(request.resume_pulsation_id.as_deref(), Some("pulse-2"));
        assert!(model.begin_reconciliation("duplicate".into()).is_none());
    }

    #[test]
    fn reconciliation_freezes_project_epoch_version_hashes_and_owner() {
        let mut model = blocked_model(vec!["pulse-1".into(), "pulse-2".into()]);
        let request = model
            .begin_reconciliation("reviewed manually".into())
            .unwrap();
        model.select("pulse-2");
        model.coordination.as_mut().unwrap().pipeline.version = 8;
        assert_eq!(request.project_id, "project-1");
        assert_eq!(request.expected_base_epoch, 2);
        assert_eq!(request.expected_pipeline_state_version, 7);
        assert_eq!(request.expected_review_token, "reviewed-files");
        assert_eq!(request.resume_pulsation_id.as_deref(), Some("pulse-1"));
    }

    #[test]
    fn review_of_terminal_effects_never_revives_a_failed_pulsation() {
        let mut model = blocked_model(vec![]);
        model.pulsations[0].status = PulsationStatus::Failed;
        let request = model
            .begin_reconciliation("current files are correct".into())
            .unwrap();
        assert!(request.resume_pulsation_id.is_none());
        model.reconciliation_busy = false;
        model.coordination.as_mut().unwrap().pipeline.base_state = BaseState::Ready;
        assert!(model.begin_reconciliation("not a review".into()).is_none());
    }
}
