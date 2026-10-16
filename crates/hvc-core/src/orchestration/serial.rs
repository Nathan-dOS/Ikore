//! Atomic result acceptance, HITL and host publication, admitted by the M2 coordinator.
use super::*;
use crate::orchestration::*;
use rusqlite::TransactionBehavior;

#[cfg(test)]
#[path = "serial_tests.rs"]
pub(super) mod tests;

fn invalid(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Validation(error.to_string())
}

/// Fails unless `checkpoint` is its pulsation's only waiting artifact review,
/// at the expected state version and input revision. Every Review read
/// (documents and brief) passes through it inside its own snapshot.
fn require_current_review(
    db: &Connection,
    checkpoint: &HitlCheckpoint,
) -> Result<(), PersistenceServiceError> {
    let valid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id
         WHERE c.id=?1 AND p.id=?2 AND p.state_version=?3
         AND c.status='waiting' AND c.purpose='artifact_review' AND p.status='checkpoint'
         AND c.input_revision=p.input_revision
         AND (c.stage=p.stage OR (c.stage='Specification' AND p.stage='SPECIFICATION'))
         AND (SELECT COUNT(*) FROM checkpoints w WHERE w.pulsation_id=p.id AND w.status='waiting')=1)",
        params![checkpoint.checkpoint_id, checkpoint.pulsation_id, checkpoint.state_version],
        |row| row.get(0),
    )?;
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "Checkpoint or version stale or ambiguous. Refresh before reviewing.",
        ))
    }
}

/// The `stage_payload_v1` content produced behind a checkpoint. `candidate`
/// keeps only the still-candidate output of a waiting review; the output of
/// a resolved review may since have been superseded.
fn stage_payload_behind(
    db: &Connection,
    checkpoint_id: &str,
    candidate: bool,
) -> Result<Option<String>, PersistenceServiceError> {
    Ok(db
        .query_row(
            "SELECT a.content FROM artifacts a
             JOIN checkpoints c ON c.pulsation_id=a.pulsation_id
               AND c.producer_job_id=a.producer_job_id AND c.generation=a.generation
               AND c.input_revision=a.input_revision AND c.stage=a.producer_stage
             WHERE c.id=?1 AND a.kind=?2 AND (?3=0 OR a.acceptance='candidate')
             ORDER BY a.output_position,a.id LIMIT 1",
            params![
                checkpoint_id,
                composition::recipes::STAGE_PAYLOAD_KIND,
                candidate
            ],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// The one field of a stored `StageHandoffV1` the revision diff quotes.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HandoffFeedback {
    revision_feedback: Option<String>,
}

/// ADR-027 part 3. When the review before this one, at the same stage and
/// input revision, ended in Request revision, its candidate's brief compared
/// with this one, and the feedback this run received as the host handed it
/// over. `None` for a stage's first candidate. A previous candidate that
/// cannot be read or briefed gives `changes: None`; nothing is inferred.
fn revision_diff(
    db: &Connection,
    checkpoint_id: &str,
    current: &composition::recipes::StagePayloadRecord,
    manifest: &crate::feature_forge::BatchManifest,
) -> Result<Option<review_brief::RevisionDiff>, PersistenceServiceError> {
    let previous: Option<(String, String)> = db
        .query_row(
            "SELECT prev.id,prev.status FROM checkpoints cur
             JOIN checkpoints prev ON prev.pulsation_id=cur.pulsation_id
               AND upper(prev.stage)=upper(cur.stage) AND prev.purpose='artifact_review'
               AND prev.input_revision=cur.input_revision AND prev.created_at<cur.created_at
             WHERE cur.id=?1 ORDER BY prev.created_at DESC LIMIT 1",
            [checkpoint_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((previous, status)) = previous else {
        return Ok(None);
    };
    if status != "revision_requested" {
        return Ok(None);
    }
    let changes = stage_payload_behind(db, &previous, false)?
        .and_then(|content| review_brief::decode_record(&content).ok())
        .and_then(|record| {
            review_brief::compare_specification_records(&record, current, manifest)
                .ok()
                .flatten()
        });
    let feedback = db
        .query_row(
            "SELECT s.handoff_json FROM stage_acceptances s
             JOIN checkpoints c ON c.producer_job_id=s.job_id AND c.generation=s.generation
             WHERE c.id=?1",
            [checkpoint_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|handoff| serde_json::from_str::<HandoffFeedback>(&handoff).ok())
        .and_then(|handoff| handoff.revision_feedback)
        .filter(|feedback| !feedback.trim().is_empty());
    Ok(Some(review_brief::RevisionDiff { feedback, changes }))
}

/// Who produced the accepted outcome and which exact bytes its publications
/// expect. The legacy stage leaves both empty; the composed stage supplies
/// its execution and the hashes it observed at admission (ADR-022).
#[derive(Debug, Clone, Default)]
pub(super) struct AcceptanceProvenance<'a> {
    pub stage_execution_id: Option<&'a str>,
    /// Keyed by the ASCII-lowercased relative path.
    pub expected_overrides: Option<&'a HashMap<String, ExpectedFileContent>>,
    /// `ModelAuthored` for the legacy stage; the composed stage validated
    /// every model field before rendering and passes `HostRendered`.
    pub language_guard: ArtifactLanguageGuard,
}

impl PersistenceService {
    pub fn get_hitl_checkpoint(
        &self,
        pulse: &str,
    ) -> Result<HitlCheckpoint, PersistenceServiceError> {
        let db = self.connection()?;
        let entries=db.prepare("SELECT p.id,c.id,p.state_version FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.status='checkpoint' AND c.status='waiting' AND c.purpose='artifact_review'")?.query_map([pulse],|r|Ok(HitlCheckpoint{pulsation_id:r.get(0)?,checkpoint_id:r.get(1)?,state_version:r.get(2)?}))?.collect::<Result<Vec<_>,_>>()?;
        if entries.len() != 1 {
            return Err(invalid("Checkpoint missing or ambiguous."));
        }
        Ok(entries.into_iter().next().expect("one checkpoint"))
    }

    pub fn apply_hitl_decision(
        &self,
        request: ApplyHitlDecisionRequest,
    ) -> Result<ApplyHitlDecisionResponse, PersistenceServiceError> {
        if request
            .feedback
            .as_ref()
            .is_some_and(|text| text.chars().count() > 16_000)
        {
            return Err(invalid("Feedback excede 16.000 caracteres."));
        }
        let fresh_component_proposals = if request.decision == HitlDecision::Approve {
            self.preflight_component_proposals(&request.pulsation_id)?
        } else {
            HashSet::new()
        };
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pulse = &request.pulsation_id;
        let current = Self::find_pulsation_by_id(&tx, pulse)?
            .ok_or_else(|| invalid("Pulsation not found."))?;
        if !matches!(request.decision, HitlDecision::Reject) {
            Self::check_project_admission(&tx, pulse)?;
        }
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id WHERE c.id=?1 AND p.id=?2 AND p.state_version=?3 AND c.status='waiting' AND c.purpose='artifact_review' AND p.status='checkpoint' AND c.input_revision=p.input_revision AND (c.stage=p.stage OR (c.stage='Specification' AND p.stage='SPECIFICATION')))",params![request.checkpoint_id,pulse,request.expected_state_version],|r|r.get(0))?;
        let waiting: i64 = tx.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [pulse],
            |r| r.get(0),
        )?;
        if !valid || waiting != 1 {
            return Err(invalid(
                "Checkpoint or version stale or ambiguous. Refresh before deciding.",
            ));
        }
        let now = Self::now_iso();
        let resolution = match request.decision {
            HitlDecision::Approve => "approved",
            HitlDecision::Reject => "rejected",
            HitlDecision::RequestRevision => "revision_requested",
        };
        tx.execute(
            "UPDATE checkpoints SET status=?2,updated_at=?3 WHERE id=?1",
            params![request.checkpoint_id, resolution, now],
        )?;
        match request.decision {
            HitlDecision::Reject => {
                Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Failed,
                    current.stage,
                    "hitl_rejected",
                    Some("Checkpoint rejected by the user."),
                )?;
                tx.execute(
                    "UPDATE pulsations SET terminal_reason='hitl_rejected' WHERE id=?1",
                    [pulse],
                )?;
                Self::mark_effects_for_review(&tx, pulse, "hitl_rejected_with_effects")?;
                tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE pulsation_id=?1 AND state IN ('pending','applying')",[pulse])?;
                Self::enqueue_learning(&tx, pulse)?;
            }
            HitlDecision::RequestRevision => {
                tx.execute("UPDATE pulsations SET integration_checkpoint_id=NULL,integration_state_version=NULL WHERE id=?1", [pulse])?;
                let target = revision_target(current.stage)
                    .ok_or_else(|| invalid("This stage has no HITL review."))?;
                Self::supersede_stage_executions(&tx, pulse, target)?;
                let target_index = catalog::STAGES
                    .iter()
                    .position(|(s, _, _)| *s == target)
                    .expect("stage");
                for (_, stage, _) in &catalog::STAGES[target_index..] {
                    tx.execute("UPDATE artifacts SET acceptance='superseded' WHERE pulsation_id=?1 AND producer_stage=?2",params![pulse,stage])?;
                }
                tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE pulsation_id=?1 AND state IN ('pending','applying') AND artifact_id IN (SELECT id FROM artifacts WHERE acceptance='superseded')",[pulse])?;
                tx.execute(
                    "UPDATE pulsations SET revision_feedback=?2 WHERE id=?1",
                    params![pulse, request.feedback],
                )?;
                let updated = Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Planning,
                    target,
                    "hitl_revision_requested",
                    Some("Revision requested; later artifacts were superseded."),
                )?;
                Self::enqueue_stage_job(&tx, &updated.id, target)?;
            }
            HitlDecision::Approve => {
                tx.execute("UPDATE artifacts SET acceptance='accepted' WHERE pulsation_id=?1 AND producer_stage=?2 AND acceptance='candidate'",params![pulse,current.stage.as_db_value()])?;
                if current.stage == PulsationStage::Specification {
                    Self::materialize_component_proposals(
                        &tx,
                        &current,
                        &fresh_component_proposals,
                    )?;
                }
                if current.stage == PulsationStage::Integrator {
                    Self::transition_pulsation(
                        &tx,
                        &current,
                        PulsationStatus::Running,
                        current.stage,
                        "integration_publication_pending",
                        Some("Approval recorded; reconciling publications."),
                    )?;
                    tx.execute("UPDATE pulsations SET integration_checkpoint_id=?2,integration_state_version=state_version WHERE id=?1",params![pulse,request.checkpoint_id])?;
                    tx.execute("UPDATE project_pipeline_state SET base_state='publishing',state_version=state_version+1 WHERE active_pulsation_id=?1",[pulse])?;
                } else {
                    let next = current
                        .stage
                        .next()
                        .ok_or_else(|| invalid("No next stage"))?;
                    let updated = Self::transition_pulsation(
                        &tx,
                        &current,
                        PulsationStatus::Planning,
                        next,
                        "hitl_approved",
                        Some("Approval recorded."),
                    )?;
                    Self::enqueue_stage_job(&tx, &updated.id, next)?;
                }
            }
        }
        tx.commit()?;
        self.wake_scheduler();
        self.reconcile_publications()?;
        let result = Self::find_pulsation_by_id(&self.connection()?, pulse)?
            .ok_or_else(|| invalid("Pulsation missing"))?;
        if result.terminal_reason.as_deref() == Some("publication_conflict") {
            return Err(invalid(
                "Approval recorded, but publication hit a conflict. Human files were kept; see the pulsation's artifacts and events.",
            ));
        }
        Ok(ApplyHitlDecisionResponse { pulsation: result })
    }

    pub(super) fn execution_fence(
        db: &Connection,
        job_id: &str,
    ) -> Result<ExecutionFence, PersistenceServiceError> {
        Ok(db.query_row("SELECT p.project_id,p.id,p.input_revision,j.generation,p.state_version FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id WHERE j.id=?1",[job_id], |r| Ok(ExecutionFence {
            project_id:r.get(0)?,pulsation_id:Some(r.get(1)?),input_revision:Some(r.get(2)?),job_id:job_id.to_owned(),generation:r.get(3)?,state_version:r.get(4)?,
        }))?)
    }

    pub(super) fn check_execution(
        db: &Connection,
        fence: &ExecutionFence,
    ) -> Result<(), PersistenceServiceError> {
        let current = Self::execution_fence(db, &fence.job_id)?;
        if let Some(pulse) = &fence.pulsation_id {
            let anticipated:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND kind='run_stage::Intake' AND job_class='background' AND pulsation_id IN (SELECT id FROM anticipatable_pulsations))",[&fence.job_id],|r|r.get(0))?;
            if !anticipated {
                Self::check_project_admission(db, pulse)?;
            }
        }
        let (status, cancel, revision): (String, Option<String>, Option<u64>) = db.query_row(
            "SELECT status,cancellation_reason,input_revision FROM jobs WHERE id=?1",
            [&fence.job_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if cancel.is_some() || revision != fence.input_revision {
            return Err(invalid(ContractError::ExecutionClosed));
        }
        validate_result_acceptance(
            fence,
            &current,
            JobStatus::from_db_value(&status).ok_or_else(|| invalid("Invalid job"))?,
            None,
        )
        .map_err(invalid)
    }

    /// Artifact content is available even when no workspace path exists. Scope both IDs.
    pub fn read_artifact_content(
        &self,
        pulsation_id: &str,
        artifact_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        self.connection()?
            .query_row(
                "SELECT content FROM artifacts WHERE id=?1 AND pulsation_id=?2",
                params![artifact_id, pulsation_id],
                |r| r.get::<_, Option<String>>(0),
            )?
            .ok_or_else(|| {
                invalid("Legacy content unavailable or ambiguous; nothing was reconstructed.")
            })
    }

    /// Read the exact candidate output behind a still-current HITL checkpoint in one snapshot.
    pub fn read_hitl_review(
        &self,
        checkpoint: &HitlCheckpoint,
    ) -> Result<Vec<HitlReviewDocument>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        require_current_review(&tx, checkpoint)?;
        let documents = tx
            .prepare(
                "SELECT a.id,a.name,a.kind,a.relative_path,a.content FROM artifacts a
             JOIN checkpoints c ON c.pulsation_id=a.pulsation_id
               AND c.producer_job_id=a.producer_job_id AND c.generation=a.generation
               AND c.input_revision=a.input_revision AND c.stage=a.producer_stage
             WHERE c.id=?1 AND a.pulsation_id=?2 AND a.acceptance='candidate'
             ORDER BY a.output_position,a.id",
            )?
            .query_map(
                params![checkpoint.checkpoint_id, checkpoint.pulsation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if documents.is_empty() || documents.iter().any(|document| document.4.is_none()) {
            return Err(invalid(
                "Checkpoint content unavailable or ambiguous; nothing was reconstructed.",
            ));
        }
        Ok(documents
            .into_iter()
            .map(
                |(artifact_id, name, kind, relative_path, content)| HitlReviewDocument {
                    artifact_id,
                    name,
                    kind,
                    relative_path,
                    content: content.expect("content checked"),
                },
            )
            .collect())
    }

    /// The derived brief of a still-current review checkpoint (ADR-027),
    /// built from the typed `stage_payload_v1` record of the exact candidate
    /// output and the frozen batch, in the same snapshot as the admission
    /// check. `None` when the stage keeps no typed payload (pipelines before
    /// v4) or has no brief yet. Approval never depends on it.
    pub fn read_checkpoint_brief(
        &self,
        checkpoint: &HitlCheckpoint,
    ) -> Result<Option<review_brief::ReviewBrief>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        require_current_review(&tx, checkpoint)?;
        let Some(content) = stage_payload_behind(&tx, &checkpoint.checkpoint_id, true)? else {
            return Ok(None);
        };
        let Some(manifest) =
            crate::feature_forge::pipeline::manifest(&tx, &checkpoint.pulsation_id)?
        else {
            return Ok(None);
        };
        let record = review_brief::decode_record(&content)
            .map_err(|error| invalid(format!("The stage payload is unreadable: {error}")))?;
        let Some(mut brief) =
            review_brief::specification_brief(&record, &manifest).map_err(invalid)?
        else {
            return Ok(None);
        };
        brief.revision = revision_diff(&tx, &checkpoint.checkpoint_id, &record, &manifest)?;
        Ok(Some(brief))
    }

    pub(super) fn load_stage_handoff(
        &self,
        fence: &ExecutionFence,
        stage: PulsationStage,
        budget: usize,
    ) -> Result<StageHandoffV1, PersistenceServiceError> {
        let db = self.connection()?;
        let pulse = fence
            .pulsation_id
            .as_deref()
            .ok_or_else(|| invalid("Stage without a pulsation"))?;
        let unavailable:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND legacy_adaptation='unavailable_or_ambiguous')",[pulse],|r|r.get(0))?;
        if unavailable {
            return Err(invalid(
                "Legacy handoff unavailable or ambiguous. Review the source stage before continuing.",
            ));
        }
        let rows=db.prepare("SELECT id,producer_job_id,producer_stage,generation,input_revision,producer_state_version,output_position,name,kind,content,content_sha256,publication_state,source_revisions_json FROM artifacts WHERE pulsation_id=?1 AND input_revision=?2 AND acceptance='accepted' AND kind NOT IN ('code_context','learning_report','component_proposal','component') ORDER BY created_at,id")?
            .query_map(params![pulse,fence.input_revision],|r| {
                let stage:String=r.get(2)?;
                let hash:String=r.get(10)?;
                let publication:String=r.get(11)?;
                let sources:String=r.get(12)?;
                Ok((stage,hash,publication,sources,ArtifactHandoff {
                    artifact_id:r.get(0)?,producer:ExecutionFence{project_id:fence.project_id.clone(),pulsation_id:Some(pulse.to_owned()),input_revision:Some(r.get(4)?),job_id:r.get(1)?,generation:r.get(3)?,state_version:r.get(5)?},
                    producer_stage:PulsationStage::Intake,output_position:r.get(6)?,name:r.get(7)?,kind:r.get(8)?,content:r.get(9)?,content_sha256:ContentSha256::of(b""),acceptance:ArtifactAcceptance::Accepted,publication:PublicationState::NotRequired,source_revisions:Vec::new(),
                }))
            })?.collect::<Result<Vec<_>,_>>()?;
        let mut artifacts = Vec::new();
        for (producer_stage, hash, publication, sources, mut artifact) in rows {
            artifact.producer_stage = catalog::parse_stage(&producer_stage)
                .ok_or_else(|| invalid("Invalid producing stage"))?;
            artifact.content_sha256 = ContentSha256::try_from(hash).map_err(invalid)?;
            artifact.publication = match publication.as_str() {
                "applied" => PublicationState::Applied,
                "not_required" => PublicationState::NotRequired,
                _ => return Err(invalid("Required publications are not reconciled yet.")),
            };
            artifact.source_revisions = serde_json::from_str(&sources).map_err(invalid)?;
            artifacts.push(artifact);
        }
        let bases=db.prepare("SELECT relative_path,content_sha256 FROM project_file_baselines WHERE project_id=?1 AND content_sha256 IS NOT NULL ORDER BY relative_path")?.query_map([&fence.project_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut seen = HashSet::new();
        let mut publication_bases = Vec::new();
        for (path, hash) in bases {
            if seen.insert(path.to_ascii_lowercase()) {
                publication_bases.push(SourceRevision {
                    project_id: fence.project_id.clone(),
                    relative_path: RelativeSourcePath::new(path).map_err(invalid)?,
                    content_sha256: ContentSha256::try_from(hash).map_err(invalid)?,
                });
            }
        }
        let clarifications = Self::clarification_history(&db, pulse, fence.input_revision)?;
        let assessment:Option<String>=db.query_row("SELECT assessment_json FROM pulsation_preparation WHERE pulsation_id=?1 AND input_revision=?2",params![pulse,fence.input_revision],|r|r.get(0)).optional()?.flatten();
        let handoff = StageHandoffV1 {
            batch_manifest: crate::feature_forge::pipeline::manifest(&db, pulse)?,
            schema_version: SchemaV1,
            recipient: fence.clone(),
            stage,
            assessment: assessment
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(invalid)?,
            context: None,
            answers: clarifications
                .iter()
                .flat_map(|c| c.answers.clone())
                .collect(),
            clarifications,
            artifacts,
            publication_bases,
            revision_feedback: db.query_row(
                "SELECT revision_feedback FROM pulsations WHERE id=?1",
                [pulse],
                |r| r.get(0),
            )?,
        };
        if stage == PulsationStage::Specification
            && let Some(batch) = &handoff.batch_manifest
        {
            for feature in &batch.features {
                for source in &feature.brief.sources {
                    crate::feature_forge::service::check_source(&db, &feature.project_id, source)?;
                }
            }
        }
        handoff.validate_for(fence, budget).map_err(invalid)?;
        Ok(handoff)
    }

    pub(super) fn accept_stage_result(
        &self,
        job: &Job,
        run_id: &str,
        handoff: &StageHandoffV1,
        execution: &StageExecutionResult,
        root: &Path,
    ) -> Result<(), PersistenceServiceError> {
        crate::agent_runtime::validate_stage_output_language(&execution.structured_output)?;
        let outcome = execution.outcome()?;
        outcome
            .validate_for(handoff.stage)
            .map_err(|e| PersistenceServiceError::ModelOutputInvalid(e.to_string()))?;
        handoff
            .validate_for(&handoff.recipient, usize::MAX)
            .map_err(invalid)?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_execution(&tx, &handoff.recipient)?;
        let pulse = handoff
            .recipient
            .pulsation_id
            .as_deref()
            .ok_or_else(|| invalid("No pulsation"))?;
        let current =
            Self::find_pulsation_by_id(&tx, pulse)?.ok_or_else(|| invalid("Pulsation missing"))?;
        if current.status != PulsationStatus::Running || current.stage != handoff.stage {
            return Err(invalid(ContractError::StaleExecution));
        }
        crate::feature_forge::pipeline::accept_evidence(
            &tx,
            pulse,
            &job.id,
            handoff.recipient.generation,
            current.stage.as_db_value(),
            &execution.structured_output,
        )?;
        Self::update_agent_run_metrics(&tx, run_id, execution)?;
        Self::persist_agent_run_output(&tx, run_id, job, execution, None)?;
        Self::persist_context_retrieval_trace(&tx, run_id, job, &execution.context_trace)?;
        Self::accept_stage_outcome_in_tx(
            &tx,
            job,
            run_id,
            handoff,
            &current,
            &outcome,
            &AcceptanceProvenance::default(),
            root,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The acceptance transaction body shared by the legacy single-call stage
    /// and the composed stage (ADR-022): durable acceptance row, artifacts and
    /// publication intents, stage disposition and job/run closure. Evidence,
    /// run metrics and output persistence are the caller's responsibility and
    /// must already be in the same transaction. Never commits.
    pub(super) fn accept_stage_outcome_in_tx(
        tx: &rusqlite::Transaction<'_>,
        job: &Job,
        run_id: &str,
        handoff: &StageHandoffV1,
        current: &Pulsation,
        outcome: &StageOutcome,
        provenance: &AcceptanceProvenance<'_>,
        root: &Path,
    ) -> Result<(), PersistenceServiceError> {
        let pulse = handoff
            .recipient
            .pulsation_id
            .as_deref()
            .ok_or_else(|| invalid("No pulsation"))?;
        let now = Self::now_iso();
        tx.execute("INSERT INTO stage_acceptances(job_id,generation,agent_run_id,fence_json,outcome_json,handoff_json,accepted_at,stage_execution_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![job.id,handoff.recipient.generation,run_id,serde_json::to_string(&handoff.recipient).map_err(invalid)?,serde_json::to_string(outcome).map_err(invalid)?,serde_json::to_string(handoff).map_err(invalid)?,now,provenance.stage_execution_id])?;
        let disposition = stage_disposition_with(current.stage, outcome, provenance.language_guard)
            .map_err(invalid)?;
        let anticipatory: bool = tx.query_row(
            "SELECT job_class='background' FROM jobs WHERE id=?1",
            [&job.id],
            |r| r.get(0),
        )?;
        if let StageOutcome::Completed { artifacts, .. } = outcome {
            let mut paths = HashSet::new();
            for (position, artifact) in artifacts.iter().enumerate() {
                if artifact.content.len() > 1024 * 1024 || artifact.content.as_bytes().contains(&0)
                {
                    return Err(PersistenceServiceError::ModelOutputInvalid(
                        "Artifact over 1 MiB or binary.".to_string(),
                    ));
                }
                let path = artifact
                    .relative_path
                    .as_ref()
                    .map(RelativeSourcePath::as_str);
                // Anticipated classification must never publish speculative workspace changes.
                let path = if current.stage == PulsationStage::Intake && anticipatory {
                    None
                } else {
                    path
                };
                if matches!(
                    artifact.kind.as_str(),
                    "code_context" | "learning_report" | "component"
                ) {
                    return Err(PersistenceServiceError::ModelOutputInvalid(
                        "Artifact kind reserved for the host collector.".to_string(),
                    ));
                }
                if let Some(path) = path {
                    publication::safe_target(root, path, false)?;
                    crate::workspace_service::validate_generated_vault_note(
                        root,
                        path,
                        &artifact.content,
                    )
                    .map_err(|error| {
                        let command_error: crate::workspace_service::WorkspaceCommandError =
                            error.into();
                        PersistenceServiceError::ModelOutputInvalid(command_error.message)
                    })?;
                    if !paths.insert(path.to_ascii_lowercase()) {
                        return Err(PersistenceServiceError::ModelOutputInvalid(
                            "Artifacts have duplicate targets.".to_string(),
                        ));
                    }
                }
                let id = Uuid::new_v4().to_string();
                let hash = ContentSha256::of(artifact.content.as_bytes());
                let acceptance = if current.stage.requires_checkpoint() {
                    "candidate"
                } else {
                    "accepted"
                };
                tx.execute("INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,updated_at,content,producer_job_id,producer_stage,generation,input_revision,producer_state_version,output_position,acceptance,publication_state) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",params![id,pulse,artifact.name,artifact.kind,path,hash.as_str(),now,artifact.content,job.id,current.stage.as_db_value(),handoff.recipient.generation,handoff.recipient.input_revision,handoff.recipient.state_version,position,acceptance,if path.is_some(){"pending"}else{"not_required"}])?;
                if let Some(path) = path {
                    // A composed stage publishes against the exact bytes it
                    // observed at admission; the legacy stage only overwrites
                    // an earlier host publication observed in THIS handoff. A
                    // newly suggested path must be absent, never blindly
                    // sampled after the LLM call.
                    let expected: Option<String> = match provenance
                        .expected_overrides
                        .and_then(|overrides| overrides.get(&path.to_ascii_lowercase()))
                    {
                        Some(ExpectedFileContent::Present(hash)) => Some(hash.as_str().to_owned()),
                        Some(ExpectedFileContent::Absent) => None,
                        None => handoff
                            .publication_bases
                            .iter()
                            .find(|base| base.relative_path.as_str().eq_ignore_ascii_case(path))
                            .map(|base| base.content_sha256.as_str().to_owned()),
                    };
                    tx.execute("INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,expected_sha256,desired_sha256,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,current.project_id,pulse,path,expected,hash.as_str(),now])?;
                }
            }
        }
        match disposition {
            StageDisposition::AwaitArtifactReview => {
                let updated = Self::transition_pulsation(
                    tx,
                    current,
                    PulsationStatus::Checkpoint,
                    current.stage,
                    "checkpoint_ready",
                    Some("Checkpoint ready for HITL review."),
                )?;
                Self::open_checkpoint(tx, &updated.id, updated.stage)?;
                tx.execute("UPDATE checkpoints SET state_version=(SELECT state_version FROM pulsations WHERE id=?1),input_revision=?2,producer_job_id=?3,generation=?4 WHERE pulsation_id=?1 AND status='waiting' AND purpose='artifact_review'",params![pulse,handoff.recipient.input_revision,job.id,handoff.recipient.generation])?;
            }
            StageDisposition::Advance(next) => {
                if current.stage == PulsationStage::Intake
                    && next == PulsationStage::Specification
                    && let StageOutcome::Completed { artifacts, .. } = outcome
                    && let Some(manifest) = &handoff.batch_manifest
                    && manifest.path_confirmation_source
                        != crate::feature_forge::PathConfirmationSource::ForgeGate
                    && Self::composed_pulsation(tx, &current.id)?
                    && Self::intake_batch_needs_path_confirmation(artifacts, manifest)
                {
                    // Preventive HITL gate (ADR-022 clarification loop, P5/A5.2):
                    // the dispatch-time gate (P1/A1.3) never confirmed a path for
                    // this batch, and Intake just classified a task as `feature`
                    // (or the host already knew one was `Foundation`) -- letting
                    // Specification run risks burning its whole budget with
                    // nowhere to anchor a PlannedDestination. Parks exactly like
                    // `AwaitArtifactReview`: same stage, same status, a
                    // checkpoint the developer resolves before Specification is
                    // ever enqueued.
                    let updated = Self::transition_pulsation(
                        tx,
                        current,
                        PulsationStatus::Checkpoint,
                        current.stage,
                        "path_confirmation_required",
                        Some(
                            "This batch has no confirmed file path; confirm one before Specification runs.",
                        ),
                    )?;
                    Self::open_checkpoint(tx, &updated.id, updated.stage)?;
                    tx.execute("UPDATE checkpoints SET kind='path_confirmation',purpose='path_confirmation',state_version=(SELECT state_version FROM pulsations WHERE id=?1),input_revision=?2,producer_job_id=?3,generation=?4 WHERE pulsation_id=?1 AND status='waiting' AND purpose='artifact_review'",params![pulse,handoff.recipient.input_revision,job.id,handoff.recipient.generation])?;
                } else {
                    let updated = Self::transition_pulsation(
                        tx,
                        current,
                        PulsationStatus::Planning,
                        next,
                        "stage_auto_advanced",
                        Some("Result accepted; waiting for required publications."),
                    )?;
                    Self::enqueue_stage_job(tx, &updated.id, updated.stage)?;
                }
            }
            StageDisposition::Fail => {
                let (code, message) = match outcome {
                    StageOutcome::Failed { code, message } => (code.as_str(), message.as_str()),
                    _ => ("model_reported_failure", "The agent reported a failure."),
                };
                // The acceptance row is durable, so this failure is never retryable;
                // it opens a checkpoint only where a revision target exists (i.e. not
                // at Intake), and otherwise stays terminal exactly as before.
                let parked = Self::record_stage_failure_checkpoint(
                    tx,
                    current,
                    current.stage,
                    &job.id,
                    Some(run_id),
                    crate::orchestration::FailureClass::ModelReportedFailure,
                    code,
                    message,
                )?;
                if !parked {
                    Self::transition_pulsation(
                        tx,
                        current,
                        PulsationStatus::Failed,
                        current.stage,
                        "stage_failed",
                        Some("The agent reported a failure; no artifact was published."),
                    )?;
                    tx.execute(
                        "UPDATE pulsations SET terminal_reason='model_reported_failure' WHERE id=?1",
                        [pulse],
                    )?;
                    Self::mark_effects_for_review(tx, pulse, "stage_failed")?;
                    Self::enqueue_learning(tx, pulse)?;
                }
            }
            StageDisposition::AwaitClarification => {
                let StageOutcome::NeedsClarification { questions } = outcome else {
                    unreachable!()
                };
                Self::persist_questions(tx, current, &handoff.recipient, questions)?;
            }
        }
        let failed = matches!(outcome, StageOutcome::Failed { .. });
        tx.execute(
            "UPDATE jobs SET status=?1,finished_at=?2,updated_at=?2 WHERE id=?3",
            params![if failed { "failed" } else { "done" }, now, job.id],
        )?;
        tx.execute(
            "UPDATE agent_runs SET status=?1,updated_at=?2 WHERE id=?3",
            params![if failed { "failed" } else { "done" }, now, run_id],
        )?;
        Ok(())
    }

    /// Whether `pulsation_id` runs the composed pipeline (ADR-022, v4). A v3
    /// batch also carries a `batch_manifest` (`PipelineVersion::uses_batch`
    /// covers both V3 and V4), so the preventive HITL gate (P5/A5.2) -- a
    /// v4-only concept -- cannot use the manifest's presence alone to scope
    /// itself to composed pulsations.
    fn composed_pulsation(
        conn: &Connection,
        pulsation_id: &str,
    ) -> Result<bool, PersistenceServiceError> {
        let pipeline_version: u8 = conn.query_row(
            "SELECT pipeline_version FROM pulsations WHERE id=?1",
            [pulsation_id],
            |r| r.get(0),
        )?;
        Ok(PipelineVersion::from_db(pipeline_version)
            .is_some_and(|version| version.supports_composition()))
    }

    /// Whether the just-accepted Intake outcome names a task the preventive
    /// HITL gate (ADR-022 clarification loop, P5/A5.2) must stop for: one the
    /// host already knows is `Foundation` (from its own manifest, no Intake
    /// needed), or one Intake itself classified `feature`. `Foundation` is a
    /// `feature_forge::TaskKind`, distinct from and not covered by Intake's own
    /// `TaskClassification` vocabulary, which has no `Foundation` variant.
    fn intake_batch_needs_path_confirmation(
        artifacts: &[StageArtifactDraft],
        manifest: &crate::feature_forge::BatchManifest,
    ) -> bool {
        let rows: Vec<(String, String)> = artifacts
            .iter()
            .map(|artifact| (artifact.kind.clone(), artifact.content.clone()))
            .collect();
        let prior =
            crate::orchestration::composition::recipes::prior_payloads(&rows).unwrap_or_default();
        let intake_tasks = prior
            .get(&crate::orchestration::composition::Stage::Intake)
            .and_then(crate::orchestration::composition::recipes::intake_of)
            .map(|payload| payload.tasks.as_slice())
            .unwrap_or(&[]);
        manifest.tasks.iter().enumerate().any(|(index, task)| {
            task.draft.kind == crate::feature_forge::TaskKind::Foundation || {
                let handle = format!("task_{}", index + 1);
                intake_tasks.iter().any(|intake_task| {
                    intake_task.task_ref.as_str() == handle
                        && intake_task.classification
                            == crate::orchestration::composition::TaskClassification::Feature
                })
            }
        })
    }

    /// Resolves a preventive path-confirmation checkpoint (ADR-022
    /// clarification loop, P5/A5.2): re-validates the batch's tasks against
    /// the supplied hints via the identical dispatch-time gate
    /// (`feature_forge::PersistenceService::resolve_path_confirmation`), and
    /// on success records the manifest's `path_confirmation_source` as
    /// `ManualHitl` (distinct from `ForgeGate`, for the Preparation view to
    /// tell the two apart later), closes the checkpoint and queues
    /// Specification. The manifest is otherwise frozen at dispatch; this is
    /// the one field the host is allowed to amend afterward.
    pub fn resolve_path_confirmation_checkpoint(
        &self,
        request: ResolvePathConfirmationRequest,
    ) -> Result<Pulsation, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = Self::find_pulsation_by_id(&tx, &request.pulsation_id)?
            .ok_or_else(|| invalid("Pulsation missing."))?;
        if current.stage != PulsationStage::Intake {
            return Err(invalid("Path clarification outside Intake."));
        }
        let (stored_checkpoint_id, state_version, purpose): (String, u64, String) = tx
            .query_row(
                "SELECT id,state_version,purpose FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
                [&request.pulsation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| invalid("Checkpoint missing or ambiguous."))?;
        if stored_checkpoint_id != request.checkpoint_id
            || purpose != "path_confirmation"
            || state_version != request.expected_state_version
        {
            return Err(invalid("Checkpoint stale or of another kind."));
        }
        let manifest_json: String = tx.query_row(
            "SELECT manifest_json FROM pulsation_batches WHERE pulsation_id=?1",
            [&request.pulsation_id],
            |r| r.get(0),
        )?;
        let mut manifest: crate::feature_forge::BatchManifest =
            serde_json::from_str(&manifest_json).map_err(|e| invalid(e.to_string()))?;
        let hints: std::collections::HashMap<&str, Option<&str>> = request
            .manual_path_hints
            .iter()
            .map(|hint| (hint.task_id.as_str(), Some(hint.path.as_str())))
            .collect();
        Self::resolve_path_confirmation(
            &tx,
            &current.project_id,
            &hints,
            &manifest.tasks,
            &manifest.features,
        )?;
        manifest.path_confirmation_source =
            crate::feature_forge::PathConfirmationSource::ManualHitl;
        let encoded = serde_json::to_string(&manifest).map_err(|e| invalid(e.to_string()))?;
        tx.execute(
            "UPDATE pulsation_batches SET manifest_json=?2,manifest_sha256=?3 WHERE pulsation_id=?1",
            params![
                request.pulsation_id,
                encoded,
                ContentSha256::of(encoded.as_bytes()).as_str()
            ],
        )?;
        tx.execute(
            "UPDATE checkpoints SET status='approved',updated_at=?2 WHERE id=?1",
            params![request.checkpoint_id, Self::now_iso()],
        )?;
        let updated = Self::transition_pulsation(
            &tx,
            &current,
            PulsationStatus::Planning,
            PulsationStage::Specification,
            "path_confirmation_resolved",
            Some("Path confirmed; Specification queued."),
        )?;
        Self::enqueue_stage_job(&tx, &updated.id, updated.stage)?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(updated)
    }

    pub(super) fn mark_effects_for_review(
        db: &Connection,
        pulse: &str,
        reason: &str,
    ) -> Result<(), PersistenceServiceError> {
        db.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason=?2,state_version=state_version+1 WHERE project_id=(SELECT project_id FROM pulsations WHERE id=?1) AND EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND publication_state IN ('applied','applying','conflict'))",params![pulse,reason])?;
        Self::release_safe_owner(db, pulse)?;
        Ok(())
    }

    /// Replays accepted intents only. Each unit holds an IMMEDIATE SQLite transaction
    /// across replace+ACK, serializing user cancellation without holding a lock over await.
    pub(super) fn reconcile_publications(&self) -> Result<(), PersistenceServiceError> {
        self.reconcile_publications_until(None).map(|_| ())
    }

    pub(super) fn reconcile_publications_until(
        &self,
        shutdown: Option<&CancellationToken>,
    ) -> Result<bool, PersistenceServiceError> {
        let token = format!("publication:{}", Uuid::new_v4());
        if shutdown.is_some_and(CancellationToken::is_cancelled) {
            return Ok(false);
        }
        if !Self::reserve_slot(&self.connection()?, JobClass::Main, &token, None, None)? {
            return Ok(false);
        }
        let _slot = self.slot_guard(token.clone()).quiet();
        let mut progressed = false;
        // Bounded reconciliation; supervisor notifications/timer continue any remaining work.
        for _ in 0..8 {
            if shutdown.is_some_and(CancellationToken::is_cancelled) {
                break;
            }
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate=tx.query_row("SELECT o.artifact_id FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id JOIN pulsations p ON p.id=o.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE o.job_class='main' AND p.id IN (SELECT id FROM coordinated_pulsations) AND j.generation=a.generation AND j.status='done' AND o.state IN ('pending','applying') AND a.acceptance='accepted' AND a.input_revision=p.input_revision ORDER BY p.created_at,p.project_id,p.queue_sequence,o.updated_at,o.artifact_id LIMIT 1",[],|r|r.get::<_,String>(0)).optional()?;
            let Some(id) = candidate else {
                tx.commit()?;
                break;
            };
            let pulse: String = tx.query_row(
                "SELECT pulsation_id FROM artifact_publications WHERE artifact_id=?1",
                [&id],
                |r| r.get(0),
            )?;
            if self.user_cancellation_pending(&pulse) {
                break;
            }
            tx.execute("UPDATE runtime_slots SET project_id=(SELECT project_id FROM artifact_publications WHERE artifact_id=?2) WHERE token=?1",params![token,id])?;
            tx.execute("UPDATE project_pipeline_state SET base_state='publishing',state_version=state_version+1 WHERE base_state='ready' AND project_id=(SELECT project_id FROM artifact_publications WHERE artifact_id=?1)",[&id])?;
            tx.execute(
                "UPDATE artifact_publications SET state='applying' WHERE artifact_id=?1",
                [&id],
            )?;
            tx.execute(
                "UPDATE artifacts SET publication_state='applying' WHERE id=?1",
                [&id],
            )?;
            tx.commit()?;
            progressed = true;
            if let Err(error) = self.apply_publication_unit(&id) {
                // A recorded terminal conflict must not prevent AppCore from opening its UI.
                // Uncommitted database/I/O errors still abort recovery and preserve the outbox.
                let recorded: bool = db.query_row(
                    "SELECT state='conflict' FROM artifact_publications WHERE artifact_id=?1",
                    [&id],
                    |r| r.get(0),
                )?;
                if !recorded {
                    return Err(error);
                }
            }
        }
        if !shutdown.is_some_and(CancellationToken::is_cancelled) {
            progressed |= self.complete_integrations()?;
            self.connection()?.execute("UPDATE project_pipeline_state SET base_state='ready',state_version=state_version+1 WHERE base_state='publishing' AND NOT EXISTS(SELECT 1 FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id WHERE o.job_class='main' AND o.project_id=project_pipeline_state.project_id AND a.acceptance='accepted' AND o.state IN ('pending','applying')) AND NOT EXISTS(SELECT 1 FROM pulsations p WHERE p.id=active_pulsation_id AND p.integration_checkpoint_id IS NOT NULL AND p.status='running')",[])?;
        }
        if progressed {
            self.wake_scheduler();
        }
        Ok(progressed)
    }

    pub(super) fn apply_publication_unit(&self, id: &str) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row=tx.query_row("SELECT p.workspace_path,o.relative_path,o.expected_sha256,a.content,o.pulsation_id,o.desired_sha256,a.content_sha256,o.job_class,a.kind FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id JOIN projects p ON p.id=o.project_id JOIN pulsations u ON u.id=o.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE j.generation=a.generation AND j.status='done' AND o.artifact_id=?1 AND o.state='applying' AND a.acceptance='accepted' AND a.input_revision=u.input_revision AND (u.status<>'failed' OR (o.job_class='background' AND a.kind IN ('learning_report','knowledge_graph_patch'))) ",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?))).optional()?;
        let Some((root, path, expected, content, pulse, desired, artifact_hash, class, kind)) = row
        else {
            tx.commit()?;
            return Ok(());
        };
        if class == "background" {
            let valid = match kind.as_str() {
                "learning_report" => {
                    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM learning_runs l JOIN pulsations p ON p.id=l.pulsation_id WHERE l.artifact_id=?1 AND l.state='publishing' AND p.status IN ('done','failed'))",[id],|r|r.get(0))?;
                    valid && Self::preparation_settings(&tx)?.learning_enabled
                }
                "knowledge_graph_patch" => {
                    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM learning_graph_proposals proposal WHERE proposal.artifact_id=?1 AND proposal.decision='approved' AND proposal.publication_state='applying')",[id],|row|row.get(0))?;
                    valid
                }
                "component" => tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM brownfield_component_notes note JOIN runtime_settings consent ON consent.key='brownfield.onboarding_consent.'||note.project_id AND consent.value='true' WHERE note.publication_artifact_id=?1 AND note.publication_state='publishing' AND note.mapping_status='proposed')",
                    [id],
                    |r| r.get(0),
                )?,
                _ => false,
            };
            if !valid {
                tx.commit()?;
                return Ok(());
            }
        } else {
            Self::check_project_admission(&tx, &pulse)?;
        }
        if class != "background" && self.user_cancellation_pending(&pulse) {
            return Ok(());
        }
        let expected = expected
            .map(ContentSha256::try_from)
            .transpose()
            .map_err(invalid)?
            .map(ExpectedFileContent::Present)
            .unwrap_or(ExpectedFileContent::Absent);
        let result = if ContentSha256::of(content.as_bytes()).as_str() != desired
            || desired != artifact_hash
        {
            Err(invalid(
                "Durable hash/content mismatch; publication refused.",
            ))
        } else {
            publication::publish(Path::new(&root), &path, &expected, &content)
        };
        match result {
            Ok(()) => {
                #[cfg(test)]
                if let Some((entered, release)) =
                    self.runtime_control.publication_gate.lock().unwrap().take()
                {
                    entered.wait();
                    release.wait();
                }
                tx.execute("UPDATE artifact_publications SET state='applied',last_error=NULL,updated_at=?2 WHERE artifact_id=?1",params![id,Self::now_iso()])?;
                tx.execute(
                    "UPDATE artifacts SET publication_state='applied' WHERE id=?1",
                    [id],
                )?;
                if class == "background" {
                    if kind == "learning_report" {
                        tx.execute("UPDATE learning_runs SET state='published',diagnostic=NULL,updated_at=?2 WHERE artifact_id=?1",params![id,Self::now_iso()])?;
                    } else if kind == "knowledge_graph_patch" {
                        tx.execute("UPDATE learning_graph_proposals SET publication_state='applied',diagnostic=NULL,updated_at=?2 WHERE artifact_id=?1",params![id,Self::now_iso()])?;
                        Self::mark_index_dirty(&tx, Path::new(&root))?;
                    } else if kind == "component" {
                        tx.execute("UPDATE brownfield_component_notes SET publication_state='published',diagnostic=NULL,updated_at=?2 WHERE publication_artifact_id=?1",params![id,Self::now_iso()])?;
                    }
                    tx.commit()?;
                    if matches!(kind.as_str(), "component" | "knowledge_graph_patch") {
                        if kind == "knowledge_graph_patch" {
                            self.document_cache.invalidate(Path::new(&root));
                        }
                        self.enqueue_embedding_path_change(Path::new(&root), &path)?;
                    }
                    return Ok(());
                }
                tx.execute("INSERT INTO project_file_baselines(project_id,relative_path,content_sha256,updated_at) SELECT project_id,relative_path,desired_sha256,?2 FROM artifact_publications WHERE artifact_id=?1 ON CONFLICT(project_id,relative_path) DO UPDATE SET content_sha256=excluded.content_sha256,updated_at=excluded.updated_at",params![id,Self::now_iso()])?;
                if path.starts_with("knowledge/obsidian/")
                    && Path::new(&path)
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                {
                    Self::mark_index_dirty(&tx, Path::new(&root))?;
                }
            }
            Err(error) => {
                tx.execute("UPDATE artifact_publications SET state='conflict',last_error=?2,updated_at=?3 WHERE artifact_id=?1",params![id,error.to_string(),Self::now_iso()])?;
                tx.execute(
                    "UPDATE artifacts SET publication_state='conflict' WHERE id=?1",
                    [id],
                )?;
                if class == "background" {
                    if kind == "learning_report" {
                        tx.execute("UPDATE learning_runs SET state='conflict',diagnostic=?2,updated_at=?3 WHERE artifact_id=?1",params![id,error.to_string(),Self::now_iso()])?;
                    } else if kind == "knowledge_graph_patch" {
                        tx.execute("UPDATE learning_graph_proposals SET publication_state='conflict',diagnostic=?2,updated_at=?3 WHERE artifact_id=?1",params![id,error.to_string(),Self::now_iso()])?;
                    } else if kind == "component" {
                        tx.execute("UPDATE brownfield_component_notes SET publication_state='conflict',diagnostic=?2,updated_at=?3 WHERE publication_artifact_id=?1",params![id,error.to_string(),Self::now_iso()])?;
                    }
                    tx.commit()?;
                    return Err(error);
                }
                let current = Self::find_pulsation_by_id(&tx, &pulse)?
                    .ok_or_else(|| invalid("Pulsation missing"))?;
                if !matches!(
                    current.status,
                    PulsationStatus::Done | PulsationStatus::Failed
                ) {
                    Self::transition_pulsation(
                        &tx,
                        &current,
                        PulsationStatus::Failed,
                        current.stage,
                        "publication_conflict",
                        Some(&error.to_string()),
                    )?;
                    tx.execute(
                        "UPDATE pulsations SET terminal_reason='publication_conflict' WHERE id=?1",
                        [&pulse],
                    )?;
                    tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',last_error=?2 WHERE pulsation_id=?1 AND status='pending'",params![pulse,error.to_string()])?;
                }
                Self::mark_effects_for_review(&tx, &pulse, "publication_conflict")?;
                tx.commit()?;
                return Err(error);
            }
        }
        tx.commit()?;
        Ok(())
    }
}
