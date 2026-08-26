use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::persistence_service::{
    Artifact, CancelPulsationJobsRequest, CreatePulsationRequest,
    GetPulsationRuntimeSnapshotRequest, ListPulsationArtifactsRequest, Pulsation,
    PulsationRuntimeSnapshot, PulsationStage, PulsationStatus,
};

use crate::hitl::HitlReviewState;
use crate::workspace::WorkspaceViewMode;

pub(crate) type PulsationsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

pub(crate) trait PulsationsBackend: Send + Sync {
    fn runtime_snapshot(&self, project_id: String) -> PulsationsFuture<PulsationRuntimeSnapshot>;
    fn artifacts(&self, pulsation_id: String) -> PulsationsFuture<Vec<Artifact>>;
    fn create(&self, project_id: String, feature_name: String) -> PulsationsFuture<Pulsation>;
    fn cancel_jobs(&self, pulsation_id: String) -> PulsationsFuture<usize>;
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
        self.awaiting_hitl = snapshot.hitl_waiting_count;
        self.running_jobs = snapshot.background_agents_count;
        self.active_pulsation_id = next_id.clone().map(Into::into);
        self.runtime_loading = false;
        self.error = None;
        let checkpoint = self
            .active()
            .filter(|pulsation| pulsation.status == PulsationStatus::Checkpoint)
            .cloned();
        self.hitl.load_checkpoint(checkpoint.as_ref());
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
        self.notice = None;
        let checkpoint = self
            .active()
            .filter(|pulsation| pulsation.status == PulsationStatus::Checkpoint)
            .cloned();
        self.hitl.load_checkpoint(checkpoint.as_ref());
        true
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
            if self
                .active()
                .is_some_and(|pulsation| pulsation.status == PulsationStatus::Checkpoint)
            {
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

pub(crate) fn status_label(status: PulsationStatus) -> &'static str {
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
}
