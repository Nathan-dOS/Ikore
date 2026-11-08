#[cfg(test)]
use hvc_core::persistence_service::CreatePulsationRequest;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::orchestration::brownfield::BrownfieldCoverageMetrics;
use hvc_core::orchestration::{
    BaseState, CleanupFailedPulsationFilesRequest, ConfirmBaseReconciliationRequest,
    FailedPulsationCleanupPlan, FailedPulsationCleanupReport, ProjectCoordinationSnapshot,
};
use hvc_core::persistence_service::{
    Artifact, CancelPulsationJobsRequest, GetPulsationRuntimeSnapshotRequest,
    ListPulsationArtifactsRequest, Pulsation, PulsationRuntimeSnapshot, PulsationStage,
    PulsationStatus,
};

use crate::failure_checkpoint::FailureCheckpointState;
use crate::hitl::HitlReviewState;
use hvc_core::orchestration::{
    ConfirmDescriptionReviewRequest, PulsationCoordinationSnapshot,
    RevisePulsationDescriptionRequest, SubmitSpecificationAnswersRequest,
};

pub(crate) type PulsationsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

pub(crate) trait PulsationsBackend: Send + Sync {
    fn review_documents(
        &self,
        checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<Vec<hvc_core::persistence_service::HitlReviewDocument>>;
    /// The derived brief of the same checkpoint (ADR-024). Backends without
    /// one report `None`, and Review opens on the documents as before.
    fn review_brief(
        &self,
        _checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<Option<hvc_core::orchestration::review_brief::ReviewBrief>> {
        Box::pin(async { Ok(None) })
    }
    /// The Visualizer's note on the same checkpoint (ADR-027 M5). Backends
    /// without one report `None`.
    fn checkpoint_visualization(
        &self,
        _checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<Option<hvc_core::orchestration::review_brief::CheckpointVisualization>>
    {
        Box::pin(async { Ok(None) })
    }
    /// What the Integrator reviewed at the same checkpoint (ADR-032), or the
    /// legacy notice. Backends without one report `None`.
    fn review_inputs(
        &self,
        _checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<
        Option<hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs>,
    > {
        Box::pin(async { Ok(None) })
    }
    /// The pulsation's revision rounds (ADR-032 M4). Backends without one
    /// report `None`, and no round line is drawn.
    fn convergence(
        &self,
        _pulsation_id: String,
    ) -> PulsationsFuture<Option<hvc_core::persistence_service::ConvergenceStatus>> {
        Box::pin(async { Ok(None) })
    }
    /// Risk and coverage of the review (ADR-034 M3). Backends without one
    /// report `None`, and no risk card is drawn.
    fn risk_status(
        &self,
        _pulsation_id: String,
    ) -> PulsationsFuture<Option<hvc_core::persistence_service::CheckpointRiskStatus>> {
        Box::pin(async { Ok(None) })
    }
    /// What approving a waiting Specification review would grant (ADR-035
    /// M1). Backends without one report `None`, and no grants card is drawn.
    fn mandate_preview(
        &self,
        _checkpoint_id: String,
    ) -> PulsationsFuture<Option<hvc_core::persistence_service::MandatePreview>> {
        Box::pin(async { Ok(None) })
    }
    fn stage_failure_detail(
        &self,
        checkpoint_id: String,
    ) -> PulsationsFuture<hvc_core::persistence_service::StageFailureDetail>;
    fn action(&self, action: PulsationAction) -> PulsationsFuture<()>;
    fn artifact_content(&self, pulse: String, artifact: String) -> PulsationsFuture<String>;
    fn runtime_snapshot(
        &self,
        project_id: String,
        detail_pulsation_id: Option<String>,
    ) -> PulsationsFuture<PulsationRuntimeSnapshot>;
    fn artifacts(&self, pulsation_id: String) -> PulsationsFuture<Vec<Artifact>>;
    /// Every open clarification checkpoint across the project (ADR-022
    /// clarification loop, P4/A4.1) -- the resolution modal's chip list.
    fn resolution_chips(
        &self,
        project_id: String,
    ) -> PulsationsFuture<Vec<hvc_core::orchestration::SpecificationQuestionSet>>;
    #[cfg(test)]
    fn create(&self, project_id: String, feature_name: String) -> PulsationsFuture<Pulsation>;
    fn cancel_jobs(&self, pulsation_id: String) -> PulsationsFuture<usize>;
    fn reconcile(&self, request: ConfirmBaseReconciliationRequest) -> PulsationsFuture<()>;
    /// Dry run of the failed-pulsation cleanup; reads the workspace, writes nothing.
    fn cleanup_preview(&self, pulsation_id: String)
    -> PulsationsFuture<FailedPulsationCleanupPlan>;
    /// Removes the files of a confirmed preview (bound by its `plan_token`).
    fn cleanup_files(
        &self,
        request: CleanupFailedPulsationFilesRequest,
    ) -> PulsationsFuture<FailedPulsationCleanupReport>;
    /// Writes the pulsation report (ADR-027 M6) under the data directory and,
    /// when `copy_to` is given, copies it there. Returns the copy's path, or
    /// the report's own. Backends without one say so.
    fn write_report(
        &self,
        _pulsation_id: String,
        _copy_to: Option<PathBuf>,
    ) -> PulsationsFuture<PathBuf> {
        Box::pin(async { Err("The pulsation report is not available here.".to_owned()) })
    }
    /// Runs one Attestation card action (ADR-036 M3) and returns the files
    /// it wrote. Backends without attestations say so.
    fn attestation(
        &self,
        _pulsation_id: String,
        _action: AttestationAction,
    ) -> PulsationsFuture<Vec<PathBuf>> {
        Box::pin(async { Err("Attestations are not available here.".to_owned()) })
    }
}

/// What the Attestation card asks the host to do (ADR-036 M3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttestationAction {
    VerifyAgain,
    SignNow,
    SealAgain,
    /// Writes the bundle there and the public key next to it.
    Save(PathBuf),
}

/// The Attestation card: one request at a time, and what the last one did
/// for the active pulsation.
#[derive(Debug, Default)]
pub(crate) struct AttestationCardState {
    pub busy: bool,
    pub notice: Option<AttestationNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttestationNotice {
    Verified,
    Signed,
    Requeued,
    Saved(Vec<PathBuf>),
    TrailerCopied,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) enum PulsationAction {
    Revise(RevisePulsationDescriptionRequest),
    Confirm(ConfirmDescriptionReviewRequest),
    Answer(SubmitSpecificationAnswersRequest),
    RetryLearning(hvc_core::orchestration::LearningActionRequest),
    DismissLearning(hvc_core::orchestration::LearningActionRequest),
    ReviewLearningGraph(hvc_core::orchestration::ReviewLearningGraphRequest),
}

pub(crate) struct CorePulsationsBackend {
    core: Arc<AppCore>,
}

impl CorePulsationsBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }

    fn worker_error(operation: &str, error: tokio::task::JoinError) -> String {
        format!("pulsation {operation} worker interrupted: {error}")
    }
}

fn observed_documents(
    mut documents: Vec<hvc_core::persistence_service::HitlReviewDocument>,
) -> Vec<hvc_core::persistence_service::HitlReviewDocument> {
    for document in &mut documents {
        document.content = hvc_core::sanitization::sanitized_observation(&document.content);
        document.name = hvc_core::sanitization::sanitized_observation(&document.name);
    }
    documents
}

/// Paths and gap subjects pass the same observation sanitization as the
/// documents before they are drawn.
fn observed_review_inputs(
    inputs: hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs,
) -> hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs {
    use hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs;
    let CheckpointReviewInputs::Recorded {
        mut manifest,
        changed_policy_files,
    } = inputs
    else {
        return inputs;
    };
    let text = hvc_core::sanitization::sanitized_observation;
    for gap in &mut manifest.gaps {
        gap.subject = text(&gap.subject);
    }
    for candidate in &mut manifest.candidates {
        candidate.path = text(&candidate.path);
    }
    for source in &mut manifest.sources {
        source.path = text(&source.path);
    }
    for check in &mut manifest.checks {
        check.status = text(&check.status);
        check.reason = check.reason.as_deref().map(text);
        check.interpretation = check.interpretation.as_deref().map(text);
        check.result = check.result.as_deref().map(text);
        check.limitations = check.limitations.iter().map(|value| text(value)).collect();
    }
    CheckpointReviewInputs::Recorded {
        manifest,
        changed_policy_files,
    }
}

pub(crate) fn has_sanitized_regions(text: &str) -> bool {
    hvc_core::secret_redactor::contains_redaction_marker(text)
        || text.contains(hvc_core::secret_redactor::OMITTED_CONTENT)
}

fn observed_brief(
    mut brief: hvc_core::orchestration::review_brief::ReviewBrief,
) -> hvc_core::orchestration::review_brief::ReviewBrief {
    use hvc_core::orchestration::review_brief::{
        BriefCheck, BriefFileGroup, BriefUnit, StageVisual,
    };
    fn text(value: &mut String) {
        *value = hvc_core::sanitization::sanitized_observation(value);
    }
    fn optional(value: &mut Option<String>) {
        if let Some(value) = value {
            text(value);
        }
    }
    fn strings(values: &mut [String]) {
        for value in values {
            text(value);
        }
    }
    fn units(values: &mut [BriefUnit]) {
        for value in values {
            text(&mut value.key);
            text(&mut value.objective);
            strings(&mut value.destinations);
            strings(&mut value.criteria);
            strings(&mut value.depends_on);
            strings(&mut value.interfaces);
        }
    }
    fn files(values: &mut [BriefFileGroup]) {
        for group in values {
            text(&mut group.root);
            for file in &mut group.files {
                text(&mut file.path);
                for interface in &mut file.interfaces {
                    text(&mut interface.description);
                }
            }
        }
    }
    fn checks(values: &mut [BriefCheck]) {
        for value in values {
            text(&mut value.check_ref);
            optional(&mut value.receipt);
        }
    }
    optional(&mut brief.objective);
    strings(&mut brief.counts);
    strings(&mut brief.verified);
    if let Some(revision) = &mut brief.revision {
        optional(&mut revision.feedback);
        if let Some(changes) = &mut revision.changes {
            for change in changes {
                text(&mut change.label);
                optional(&mut change.before);
                optional(&mut change.after);
            }
        }
    }
    for item in &mut brief.judgement {
        text(&mut item.label);
        text(&mut item.text);
        optional(&mut item.detail);
        strings(&mut item.refs);
    }
    match &mut brief.visual {
        StageVisual::Specification(visual) => {
            strings(&mut visual.exclusions);
            files(&mut visual.files);
            units(&mut visual.units);
            for interface in &mut visual.unattached_interfaces {
                text(&mut interface.description);
            }
            for task in &mut visual.tasks {
                text(&mut task.task_id);
                text(&mut task.title);
                for criterion in &mut task.criteria {
                    text(&mut criterion.criterion_id);
                    text(&mut criterion.description);
                    text(&mut criterion.requirement);
                    text(&mut criterion.verification_intent);
                    strings(&mut criterion.checks);
                    optional(&mut criterion.not_applicable_reason);
                }
            }
        }
        StageVisual::Contract(visual) => {
            units(&mut visual.units);
            for interface in &mut visual.interfaces {
                text(&mut interface.key);
                text(&mut interface.signature);
                strings(&mut interface.invariants);
                strings(&mut interface.sources);
                strings(&mut interface.criteria);
                strings(&mut interface.units);
            }
        }
        StageVisual::Test(visual) => {
            checks(&mut visual.checks);
            files(&mut visual.files);
            for coverage in &mut visual.coverage {
                text(&mut coverage.task_id);
                text(&mut coverage.criterion_id);
                text(&mut coverage.description);
                text(&mut coverage.task_title);
                text(&mut coverage.explanation);
                strings(&mut coverage.tests);
                strings(&mut coverage.declared_checks);
            }
        }
        StageVisual::Integrator(visual) => {
            checks(&mut visual.checks);
            strings(&mut visual.blockers);
            for verdict in &mut visual.verdicts {
                text(&mut verdict.task_id);
                text(&mut verdict.criterion_id);
                text(&mut verdict.description);
                text(&mut verdict.task_title);
                text(&mut verdict.explanation);
                strings(&mut verdict.checks);
            }
            for publication in &mut visual.publications {
                text(&mut publication.path);
            }
        }
    }
    brief
}

fn observed_visualization(
    mut visualization: hvc_core::orchestration::review_brief::CheckpointVisualization,
) -> hvc_core::orchestration::review_brief::CheckpointVisualization {
    visualization.diagnostic = visualization
        .diagnostic
        .map(|text| hvc_core::sanitization::sanitized_observation(&text));
    if let Some(note) = &mut visualization.note {
        note.headline = hvc_core::sanitization::sanitized_observation(&note.headline);
        for item in &mut note.notes {
            item.text = hvc_core::sanitization::sanitized_observation(&item.text);
            for reference in &mut item.refs {
                *reference = hvc_core::sanitization::sanitized_observation(reference);
            }
        }
    }
    visualization
}

impl PulsationsBackend for CorePulsationsBackend {
    fn review_documents(
        &self,
        checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<Vec<hvc_core::persistence_service::HitlReviewDocument>> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().read_hitl_review(&checkpoint))
                .await
                .map_err(|e| Self::worker_error("review", e))?
                .map(observed_documents)
                .map_err(|e| e.to_string())
        })
    }
    fn review_brief(
        &self,
        checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<Option<hvc_core::orchestration::review_brief::ReviewBrief>> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().read_checkpoint_brief(&checkpoint))
                .await
                .map_err(|e| Self::worker_error("review brief", e))?
                .map(|brief| brief.map(observed_brief))
                .map_err(|e| e.to_string())
        })
    }
    fn checkpoint_visualization(
        &self,
        checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<Option<hvc_core::orchestration::review_brief::CheckpointVisualization>>
    {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .read_checkpoint_visualization(&checkpoint)
                })
                .await
                .map_err(|e| Self::worker_error("visualizer note", e))?
                .map(|note| note.map(observed_visualization))
                .map_err(|e| e.to_string())
        })
    }
    fn review_inputs(
        &self,
        checkpoint: hvc_core::persistence_service::HitlCheckpoint,
    ) -> PulsationsFuture<
        Option<hvc_core::orchestration::composition::review_boundary::CheckpointReviewInputs>,
    > {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .read_checkpoint_review_inputs(&checkpoint)
                })
                .await
                .map_err(|e| Self::worker_error("review inputs", e))?
                .map(|inputs| inputs.map(observed_review_inputs))
                .map_err(|e| e.to_string())
        })
    }
    fn convergence(
        &self,
        pulsation_id: String,
    ) -> PulsationsFuture<Option<hvc_core::persistence_service::ConvergenceStatus>> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().convergence_status(&pulsation_id))
                .await
                .map_err(|e| Self::worker_error("revision rounds", e))?
                .map(Some)
                .map_err(|e| e.to_string())
        })
    }
    fn risk_status(
        &self,
        pulsation_id: String,
    ) -> PulsationsFuture<Option<hvc_core::persistence_service::CheckpointRiskStatus>> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().checkpoint_risk_status(&pulsation_id))
                .await
                .map_err(|e| Self::worker_error("risk and coverage", e))?
                .map(Some)
                .map_err(|e| e.to_string())
        })
    }
    fn mandate_preview(
        &self,
        checkpoint_id: String,
    ) -> PulsationsFuture<Option<hvc_core::persistence_service::MandatePreview>> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().mandate_preview(&checkpoint_id))
                .await
                .map_err(|e| Self::worker_error("approval grants", e))?
                .map_err(|e| e.to_string())
        })
    }
    fn stage_failure_detail(
        &self,
        checkpoint_id: String,
    ) -> PulsationsFuture<hvc_core::persistence_service::StageFailureDetail> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence().read_stage_failure_detail(&checkpoint_id)
                })
                .await
                .map_err(|e| Self::worker_error("failure detail", e))?
                .map_err(|e| e.to_string())
        })
    }
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
                    PulsationAction::ReviewLearningGraph(request) => {
                        core.persistence().review_learning_graph(request)
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
                .map_err(|e| Self::worker_error("action", e))?
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
                .map_err(|e| Self::worker_error("content", e))?
                .map(|content| hvc_core::sanitization::sanitized_observation(&content))
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
                .map_err(|e| Self::worker_error("reconciliation", e))?
        })
    }

    fn runtime_snapshot(
        &self,
        project_id: String,
        detail_pulsation_id: Option<String>,
    ) -> PulsationsFuture<PulsationRuntimeSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .get_pulsation_runtime_snapshot_for_detail(
                            GetPulsationRuntimeSnapshotRequest { project_id },
                            detail_pulsation_id.as_deref(),
                        )
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
                .map_err(|error| Self::worker_error("artifacts", error))?
        })
    }

    fn resolution_chips(
        &self,
        project_id: String,
    ) -> PulsationsFuture<Vec<hvc_core::orchestration::SpecificationQuestionSet>> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .list_open_specification_checkpoints(&project_id)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("resolution chips", error))?
        })
    }

    #[cfg(test)]
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
                .map_err(|error| Self::worker_error("creation", error))?
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

    fn cleanup_preview(
        &self,
        pulsation_id: String,
    ) -> PulsationsFuture<FailedPulsationCleanupPlan> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .preview_failed_pulsation_cleanup(&pulsation_id)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("cleanup preview", error))?
        })
    }

    fn cleanup_files(
        &self,
        request: CleanupFailedPulsationFilesRequest,
    ) -> PulsationsFuture<FailedPulsationCleanupReport> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .cleanup_failed_pulsation_files(request)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| Self::worker_error("cleanup", error))?
        })
    }
    fn write_report(
        &self,
        pulsation_id: String,
        copy_to: Option<PathBuf>,
    ) -> PulsationsFuture<PathBuf> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let written = core
                        .persistence()
                        .write_pulsation_report(&pulsation_id)
                        .map_err(|error| error.to_string())?;
                    match copy_to {
                        Some(target) => std::fs::copy(&written, &target)
                            .map(|_| target)
                            .map_err(|error| format!("The copy could not be saved: {error}")),
                        None => Ok(written),
                    }
                })
                .await
                .map_err(|error| Self::worker_error("pulsation report", error))?
        })
    }
    fn attestation(
        &self,
        pulsation_id: String,
        action: AttestationAction,
    ) -> PulsationsFuture<Vec<PathBuf>> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let persistence = core.persistence();
                    let host = |error: hvc_core::persistence_service::PersistenceServiceError| {
                        error.to_string()
                    };
                    let disk = |error: std::io::Error| {
                        format!("The attestation could not be saved: {error}")
                    };
                    match action {
                        AttestationAction::VerifyAgain => persistence
                            .verify_attestation(&pulsation_id)
                            .map(|_| Vec::new())
                            .map_err(host),
                        AttestationAction::SignNow => persistence
                            .sign_attestation(&pulsation_id)
                            .map(|_| Vec::new())
                            .map_err(host),
                        AttestationAction::SealAgain => persistence
                            .retry_attestation_seal(&pulsation_id)
                            .map(|_| Vec::new())
                            .map_err(host),
                        AttestationAction::Save(bundle) => {
                            let export = persistence
                                .export_attestation(&pulsation_id)
                                .map_err(host)?;
                            let key = bundle.with_file_name(&export.key_file_name);
                            std::fs::write(&bundle, &export.bundle).map_err(disk)?;
                            std::fs::write(&key, &export.public_key_pem).map_err(disk)?;
                            Ok(vec![bundle, key])
                        }
                    }
                })
                .await
                .map_err(|error| Self::worker_error("attestation", error))?
        })
    }
}

/// The "Leftover files" card of a failed pulsation: the preview the operator
/// confirms, and the two-click arming of the removal itself.
#[derive(Debug, Default)]
pub(crate) struct CleanupState {
    pub plan: Option<FailedPulsationCleanupPlan>,
    pub loading: bool,
    pub busy: bool,
    /// The first click arms; the second removes. Leaving the pulsation disarms.
    pub armed: bool,
    pub error: Option<String>,
}

impl CleanupState {
    pub(crate) fn can_remove(&self) -> bool {
        !self.busy && !self.loading && self.plan.as_ref().is_some_and(|plan| !plan.is_noop())
    }
}

/// The Pulsation report card (ADR-027 M6): one request at a time, and what
/// the last one did for the active pulsation.
#[derive(Debug, Default)]
pub(crate) struct ReportState {
    pub busy: bool,
    pub notice: Option<ReportNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReportNotice {
    /// Written and handed to the system's default application for HTML.
    Opened(PathBuf),
    /// Written and copied where the developer asked.
    Saved(PathBuf),
    Failed(String),
}

/// The report exists once the Integrator checkpoint has opened, and when a
/// pulsation ended (ADR-027 §5). The host refuses the same cases.
pub(crate) fn report_available(pulsation: &Pulsation) -> bool {
    matches!(
        pulsation.status,
        PulsationStatus::Done | PulsationStatus::Failed
    ) || (pulsation.stage == PulsationStage::Integrator
        && pulsation.status == PulsationStatus::Checkpoint)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PulsationDetailSection {
    /// The HITL approval surface for the active pulsation's checkpoint. Selected automatically when
    /// a checkpoint newly appears (see `apply_snapshot`/`select`); formerly a separate Workspace view
    /// mode (see the UI flow plan Part 1.2).
    Review,
    #[default]
    Overview,
    Preparation,
    Artifacts,
}

#[derive(Default)]
pub struct PulsationsModel {
    pub(crate) usage: crate::usage::UsageState,
    /// User choices survive view navigation and runtime polling for this project.
    pub(crate) group_expansion: std::collections::HashMap<&'static str, bool>,
    /// Status groups the developer asked to see in full; the rest render their newest
    /// `GROUP_RENDER_LIMIT` pulsations (plus the selected one).
    pub(crate) groups_showing_all: std::collections::HashSet<&'static str>,
    pub detail_section: PulsationDetailSection,
    pub brownfield_coverage: Option<BrownfieldCoverageMetrics>,
    pub details: Vec<PulsationCoordinationSnapshot>,
    pub action_busy: bool,
    pub artifact_preview: Option<(String, String)>,
    pub coordination: Option<ProjectCoordinationSnapshot>,
    pub reconciliation_busy: bool,
    pub hitl_checkpoints: Vec<hvc_core::persistence_service::HitlCheckpoint>,
    pub failure_checkpoints: Vec<hvc_core::persistence_service::StageFailureCheckpoint>,
    pub project_id: Option<SharedString>,
    pub pulsations: Vec<Pulsation>,
    pub active_pulsation_id: Option<SharedString>,
    pub artifacts: Vec<Artifact>,
    pub running_jobs: usize,
    /// Who holds the runtime slots (the header's "now running" readout). Main first.
    pub active_jobs: Vec<hvc_core::persistence_service::ActiveRuntimeJob>,
    pub awaiting_hitl: usize,
    /// Counted apart from `awaiting_hitl` so a parked failure cannot suppress the
    /// notification for an artifact review that opens after it.
    pub awaiting_failure: usize,
    pub runtime_loading: bool,
    pub artifacts_loading: bool,
    pub error: Option<String>,
    pub(crate) validation_error: Option<(&'static str, String)>,
    pub notice: Option<String>,
    /// Expands the Review section's diff surface to full width, hiding the pulsation list. Formerly
    /// a Workspace concept (`diff_expanded`); moved here with the HITL surface.
    pub diff_expanded: bool,
    /// Receipt currently inspected in the composed-stage panel.
    pub(crate) expanded_validation_receipt: Option<String>,
    /// Layers the developer opened in the Layers panel (ADR-035 M2).
    pub(crate) layers_expanded:
        std::collections::HashSet<hvc_core::orchestration::layers::LayerKind>,
    /// The composed execution under "Execution details", collapsed by
    /// default below the Layers panel.
    pub(crate) execution_details_open: bool,
    pub(crate) hitl: HitlReviewState,
    pub(crate) failure: FailureCheckpointState,
    /// The backend the active pulsation's current stage silently failed
    /// over to (see `elect_next_backend` in hvc-core), when there is one.
    /// `None` means the stage is running on the project's configured
    /// default backend.
    pub backend_override: Option<hvc_core::terminals::BackendOverrideStatus>,
    pub backend_override_busy: bool,
    pub(crate) cleanup: CleanupState,
    pub(crate) report: ReportState,
    pub(crate) attestation: AttestationCardState,
}

impl PulsationsModel {
    pub(crate) fn group_expanded(&self, status: PulsationStatus) -> bool {
        self.group_expansion
            .get(status_group_id(status))
            .copied()
            .unwrap_or(status == PulsationStatus::Running)
    }

    pub(crate) fn toggle_group(&mut self, status: PulsationStatus) {
        self.group_expansion
            .insert(status_group_id(status), !self.group_expanded(status));
    }

    pub(crate) fn detail(&self) -> Option<&PulsationCoordinationSnapshot> {
        let active = self.active()?;
        self.details.iter().find(|d| d.pulsation_id == active.id)
    }
    pub(crate) fn reset(&mut self, project_id: Option<String>) {
        let usage = std::mem::take(&mut self.usage).invalidated();
        *self = Self {
            usage,
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

    /// The waiting failure checkpoint for the active pulsation, if any. A pulsation
    /// holds at most one waiting checkpoint of any purpose, so this and the HITL
    /// review are mutually exclusive by construction.
    fn active_failure_checkpoint(
        &self,
    ) -> Option<hvc_core::persistence_service::StageFailureCheckpoint> {
        let active = self.active()?;
        if active.status != PulsationStatus::Checkpoint {
            return None;
        }
        self.failure_checkpoints
            .iter()
            .find(|c| c.pulsation_id == active.id)
            .cloned()
    }

    pub(crate) fn apply_snapshot(
        &mut self,
        project_id: String,
        mut snapshot: PulsationRuntimeSnapshot,
        preferred_id: Option<&str>,
    ) -> Option<String> {
        // Presentation order only: persisted queue sequence and execution admission stay intact.
        snapshot.pulsations.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let current_id = self.active_pulsation_id.as_ref().map(AsRef::as_ref);
        let next_id = preferred_id
            .filter(|id| snapshot.pulsations.iter().any(|item| item.id == *id))
            .or_else(|| {
                current_id.filter(|id| snapshot.pulsations.iter().any(|item| item.id == *id))
            })
            .map(str::to_owned)
            .or_else(|| snapshot.pulsations.first().map(|item| item.id.clone()));
        let pulse_changed = current_id != next_id.as_deref()
            || self.project_id.as_ref().map(AsRef::as_ref) != Some(project_id.as_str());
        if pulse_changed {
            self.validation_error = None;
            self.expanded_validation_receipt = None;
            self.detail_section = PulsationDetailSection::Overview;
            self.artifacts.clear();
            self.artifact_preview = None;
            self.backend_override = None;
            self.cleanup = CleanupState::default();
            self.report = ReportState::default();
            self.attestation = AttestationCardState::default();
        }
        let had_checkpoint = self.hitl.pulsation_id.is_some() || self.failure.checkpoint.is_some();
        self.project_id = Some(project_id.into());
        self.pulsations = snapshot.pulsations;
        self.details = snapshot.details;
        self.hitl_checkpoints = snapshot.hitl_checkpoints;
        self.failure_checkpoints = snapshot.failure_checkpoints;
        self.awaiting_hitl = snapshot.hitl_waiting_count;
        self.awaiting_failure = snapshot.failure_waiting_count;
        self.running_jobs = snapshot.main_agents_count + snapshot.background_agents_count;
        self.active_jobs = std::mem::take(&mut snapshot.active_jobs);
        self.coordination = snapshot.coordination;
        self.brownfield_coverage = snapshot.brownfield_coverage;
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
        let expected_checkpoint = self
            .hitl_checkpoints
            .iter()
            .find(|c| Some(c.pulsation_id.as_str()) == checkpoint.as_ref().map(|p| p.id.as_str()))
            .cloned();
        self.hitl
            .load_checkpoint(checkpoint.as_ref(), expected_checkpoint);
        let failure = self.active_failure_checkpoint();
        let waiting = checkpoint.is_some() || failure.is_some();
        self.failure.load_checkpoint(failure);
        // Take the user to Review exactly when a checkpoint newly appears for the active
        // pulsation, or when the active pulsation changed while a checkpoint was already waiting.
        // An ordinary refresh that doesn't change checkpoint availability leaves the user's chosen
        // section alone (see the UI flow plan Part 1.2). A parked failure is a checkpoint for this
        // purpose: it is exactly as urgent as a review.
        if waiting && (pulse_changed || !had_checkpoint) {
            self.detail_section = PulsationDetailSection::Review;
        }
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
        let pulse_changed =
            self.active_pulsation_id.as_ref().map(AsRef::as_ref) != Some(pulsation_id);
        if pulse_changed {
            self.validation_error = None;
            self.expanded_validation_receipt = None;
            self.detail_section = PulsationDetailSection::Overview;
            self.cleanup = CleanupState::default();
            self.report = ReportState::default();
            self.attestation = AttestationCardState::default();
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
        let expected_checkpoint = self
            .hitl_checkpoints
            .iter()
            .find(|c| Some(c.pulsation_id.as_str()) == checkpoint.as_ref().map(|p| p.id.as_str()))
            .cloned();
        self.hitl
            .load_checkpoint(checkpoint.as_ref(), expected_checkpoint);
        let failure = self.active_failure_checkpoint();
        let waiting = checkpoint.is_some() || failure.is_some();
        self.failure.load_checkpoint(failure);
        // Selecting a pulsation that is already at a checkpoint takes the user straight to Review
        // (see the UI flow plan Part 1.2).
        if pulse_changed && waiting {
            self.detail_section = PulsationDetailSection::Review;
        }
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
}

pub(crate) const STATUS_GROUPS: [PulsationStatus; 6] = [
    PulsationStatus::Running,
    PulsationStatus::Checkpoint,
    PulsationStatus::Planning,
    PulsationStatus::Idle,
    PulsationStatus::Failed,
    PulsationStatus::Done,
];

pub(crate) fn status_group_id(status: PulsationStatus) -> &'static str {
    match status {
        PulsationStatus::Running => "running",
        PulsationStatus::Checkpoint => "checkpoint",
        PulsationStatus::Planning => "planning",
        PulsationStatus::Idle => "idle",
        PulsationStatus::Failed => "failed",
        PulsationStatus::Done => "done",
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
        // Everything the host writes as a terminal_reason gets a name here; a bare
        // "Failed" tells the developer nothing about whether it was their doing,
        // the provider's, or the agent giving up.
        match terminal_reason {
            Some("user_cancel") => return "Cancelled",
            Some("publication_conflict") => return "Publication conflict",
            Some("hitl_rejected") => return "Rejected",
            Some("retry_budget_exhausted") => return "Retries exhausted",
            Some("provider_configuration") => return "Provider not configured",
            Some("provider_transport") => return "Provider failed",
            Some("infrastructure") => return "Local storage failed",
            Some("executor_setup") | Some("executor_setup_failed") => return "Executor failed",
            Some("host_contract") => return "Contract violated",
            Some("model_reported_failure") => return "Agent gave up",
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

    #[test]
    fn observational_review_projects_content_without_mutating_the_original_candidate() {
        use hvc_core::orchestration::review_brief::*;
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        let original = hvc_core::persistence_service::HitlReviewDocument {
            artifact_id: "stable-artifact".into(),
            name: "Candidate".into(),
            kind: "code".into(),
            relative_path: Some("src/lib.rs".into()),
            content: format!("const KEY: &str = \"{secret}\";\r\nfn updated() {{}}\r\n"),
        };
        let projected = observed_documents(vec![original.clone()]).remove(0);
        assert!(original.content.contains(secret));
        assert!(!projected.content.contains(secret));
        assert!(has_sanitized_regions(&projected.content));
        assert_eq!(projected.artifact_id, original.artifact_id);
        assert_eq!(projected.relative_path, original.relative_path);
        assert!(projected.content.ends_with("fn updated() {}\r\n"));
        let brief = ReviewBrief {
            stage: PulsationStage::Integrator,
            next_stage: None,
            objective: Some(secret.into()),
            counts: vec!["1 criterion".into()],
            verified: vec!["Host checks passed".into()],
            revision: Some(RevisionDiff {
                feedback: Some(secret.into()),
                changes: None,
            }),
            judgement: vec![JudgementItem {
                kind: JudgementKind::Decision,
                label: "D1".into(),
                text: secret.into(),
                detail: Some(secret.into()),
                refs: vec!["src/lib.rs".into()],
            }],
            visual: StageVisual::Integrator(IntegratorVisual {
                verdicts: vec![],
                blockers: vec![secret.into()],
                checks: vec![],
                publications: vec![],
            }),
        };
        let observed = observed_brief(brief.clone());
        assert!(serde_json::to_string(&brief).unwrap().contains(secret));
        assert!(!serde_json::to_string(&observed).unwrap().contains(secret));
        assert_eq!(observed.stage, brief.stage);
        assert_eq!(observed.counts, brief.counts);
        assert_eq!(observed.judgement[0].label, brief.judgement[0].label);
    }

    #[test]
    fn newest_first_preserves_selection_and_group_choices_across_transitions() {
        let mut model = PulsationsModel::default();
        assert!(model.group_expanded(PulsationStatus::Running));
        for status in STATUS_GROUPS.into_iter().skip(1) {
            assert!(!model.group_expanded(status));
        }
        model.toggle_group(PulsationStatus::Running);
        model.toggle_group(PulsationStatus::Failed);
        let mut old = pulsation("old", PulsationStatus::Done);
        old.created_at = "2026-09-01T00:00:00Z".into();
        let mut newest = pulsation("new", PulsationStatus::Running);
        newest.created_at = "2026-09-09T00:00:00Z".into();
        let snapshot = PulsationRuntimeSnapshot {
            pulsations: vec![old, newest],
            hitl_checkpoints: vec![],
            details: vec![],
            coordination: None,
            main_agents_count: 1,
            background_agents_count: 0,
            hitl_waiting_count: 0,
            failure_checkpoints: Vec::new(),
            failure_waiting_count: 0,
            active_jobs: Vec::new(),
            brownfield_coverage: None,
        };
        model.apply_snapshot("project-1".into(), snapshot.clone(), None);
        assert_eq!(
            model
                .pulsations
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "old"]
        );
        assert_eq!(model.active_id().as_deref(), Some("new"));
        model.select("old");
        let mut changed = snapshot;
        changed.pulsations[1].status = PulsationStatus::Failed;
        model.apply_snapshot("project-1".into(), changed, None);
        assert_eq!(model.active_id().as_deref(), Some("old"));
        assert!(!model.group_expanded(PulsationStatus::Running));
        assert!(model.group_expanded(PulsationStatus::Failed));
        assert_eq!(model.pulsations[0].status, PulsationStatus::Failed);
    }

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
                active_jobs: Vec::new(),
                brownfield_coverage: Some(BrownfieldCoverageMetrics {
                    frontier_initialized: true,
                    inventory_known_sources: 4,
                    eligible_source_denominator: 3,
                    ignored_sources: 1,
                    extracted_valid_sources: 2,
                    extracted_partial_sources: 1,
                    extracted_stale_sources: 0,
                    extraction_failed_sources: 0,
                    extraction_progress_basis_points: 6_666,
                    documentation_proposed_sources: 1,
                    documentation_validated_sources: 0,
                    documentation_stale_sources: 0,
                    ..Default::default()
                }),
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
                failure_checkpoints: Vec::new(),
                failure_waiting_count: 0,
                background_agents_count: 2,
            },
            Some("pulse-1"),
        );
        model.artifacts_loading = true;
        model.finish_artifacts("pulse-1", Ok(vec![artifact("artifact-1", "Login plan")]));

        assert_eq!(active.as_deref(), Some("pulse-1"));
        assert_eq!(model.awaiting_hitl, 1);
        assert_eq!(model.running_jobs, 2);
        assert_eq!(model.detail_section, PulsationDetailSection::Review);
        assert_eq!(model.artifacts[0].name, "Login plan");
        assert_eq!(model.hitl.pulsation_id.as_deref(), Some("pulse-1"));
        assert_eq!(
            model
                .brownfield_coverage
                .as_ref()
                .unwrap()
                .eligible_source_denominator,
            3
        );
    }

    #[test]
    fn refreshes_artifacts_for_selected_pulsation() {
        let mut model = PulsationsModel::default();
        model.apply_snapshot(
            "project-1".into(),
            PulsationRuntimeSnapshot {
                active_jobs: Vec::new(),
                brownfield_coverage: None,
                details: Vec::new(),
                coordination: None,
                main_agents_count: 0,
                hitl_checkpoints: Vec::new(),
                pulsations: vec![pulsation("pulse-1", PulsationStatus::Running)],
                hitl_waiting_count: 0,
                failure_checkpoints: Vec::new(),
                failure_waiting_count: 0,
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
