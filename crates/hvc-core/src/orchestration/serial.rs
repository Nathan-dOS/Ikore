//! M1 transaction boundary, used by the existing (single-lane) worker.
use super::*;
use crate::orchestration::*;
use rusqlite::TransactionBehavior;

#[cfg(test)]
#[path = "serial_tests.rs"]
mod tests;

fn invalid(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Validation(error.to_string())
}

impl PersistenceService {
    pub fn get_hitl_checkpoint(
        &self,
        pulse: &str,
    ) -> Result<HitlCheckpoint, PersistenceServiceError> {
        let db = self.connection()?;
        let entries=db.prepare("SELECT p.id,c.id,p.state_version FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.status='checkpoint' AND c.status='waiting'")?.query_map([pulse],|r|Ok(HitlCheckpoint{pulsation_id:r.get(0)?,checkpoint_id:r.get(1)?,state_version:r.get(2)?}))?.collect::<Result<Vec<_>,_>>()?;
        if entries.len() != 1 {
            return Err(invalid("Checkpoint ausente ou ambíguo."));
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
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pulse = &request.pulsation_id;
        let current = Self::find_pulsation_by_id(&tx, pulse)?
            .ok_or_else(|| invalid("Pulsação não encontrada."))?;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id WHERE c.id=?1 AND p.id=?2 AND p.state_version=?3 AND c.status='waiting' AND c.kind='artifact_review' AND p.status='checkpoint' AND c.input_revision=p.input_revision AND (c.stage=p.stage OR (c.stage='Specification' AND p.stage='SPECIFICATION')))",params![request.checkpoint_id,pulse,request.expected_state_version],|r|r.get(0))?;
        let waiting: i64 = tx.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [pulse],
            |r| r.get(0),
        )?;
        if !valid || waiting != 1 {
            return Err(invalid(
                "Checkpoint/versão obsoleto ou ambíguo. Atualize antes de decidir.",
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
                    Some("Checkpoint rejeitado pelo usuário."),
                )?;
                tx.execute(
                    "UPDATE pulsations SET terminal_reason='hitl_rejected' WHERE id=?1",
                    [pulse],
                )?;
                Self::mark_effects_for_review(&tx, pulse, "hitl_rejected_with_effects")?;
                tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE pulsation_id=?1 AND state IN ('pending','applying')",[pulse])?;
            }
            HitlDecision::RequestRevision => {
                let target = revision_target(current.stage)
                    .ok_or_else(|| invalid("Este estágio não oferece revisão HITL."))?;
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
                    Some("Revisão solicitada; artifacts posteriores foram superados."),
                )?;
                Self::enqueue_stage_job(&tx, &updated.id, target)?;
            }
            HitlDecision::Approve => {
                tx.execute("UPDATE artifacts SET acceptance='accepted' WHERE pulsation_id=?1 AND producer_stage=?2 AND acceptance='candidate'",params![pulse,current.stage.as_db_value()])?;
                if current.stage == PulsationStage::Integrator {
                    Self::transition_pulsation(
                        &tx,
                        &current,
                        PulsationStatus::Running,
                        current.stage,
                        "integration_publication_pending",
                        Some("Aprovação registrada; reconciliando publicações."),
                    )?;
                } else {
                    let next = current
                        .stage
                        .next()
                        .ok_or_else(|| invalid("Sem próximo estágio"))?;
                    let updated = Self::transition_pulsation(
                        &tx,
                        &current,
                        PulsationStatus::Planning,
                        next,
                        "hitl_approved",
                        Some("Aprovação registrada."),
                    )?;
                    Self::enqueue_stage_job(&tx, &updated.id, next)?;
                }
            }
        }
        tx.commit()?;
        self.reconcile_publications()?;
        let result = Self::find_pulsation_by_id(&self.connection()?, pulse)?
            .ok_or_else(|| invalid("Pulsação ausente"))?;
        if result.terminal_reason.as_deref() == Some("publication_conflict") {
            return Err(invalid(
                "Aprovação registrada, mas a publicação encontrou conflito. Os arquivos humanos foram preservados; consulte os artifacts e eventos da pulsação.",
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

    fn check_execution(
        db: &Connection,
        fence: &ExecutionFence,
    ) -> Result<(), PersistenceServiceError> {
        let current = Self::execution_fence(db, &fence.job_id)?;
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
            JobStatus::from_db_value(&status).ok_or_else(|| invalid("Job inválido"))?,
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
                invalid("Conteúdo legado indisponível/ambíguo; nenhuma reconstrução foi inventada.")
            })
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
            .ok_or_else(|| invalid("Estágio sem pulsação"))?;
        let unavailable:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND legacy_adaptation='unavailable_or_ambiguous')",[pulse],|r|r.get(0))?;
        if unavailable {
            return Err(invalid(
                "Handoff legado indisponível ou ambíguo. Revise a etapa de origem antes de continuar.",
            ));
        }
        let rows=db.prepare("SELECT id,producer_job_id,producer_stage,generation,input_revision,producer_state_version,output_position,name,kind,content,content_sha256,publication_state,source_revisions_json FROM artifacts WHERE pulsation_id=?1 AND input_revision=?2 AND acceptance='accepted' ORDER BY created_at,id")?
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
                .ok_or_else(|| invalid("Estágio produtor inválido"))?;
            artifact.content_sha256 = ContentSha256::try_from(hash).map_err(invalid)?;
            artifact.publication = match publication.as_str() {
                "applied" => PublicationState::Applied,
                "not_required" => PublicationState::NotRequired,
                _ => return Err(invalid("Publicações obrigatórias ainda não reconciliadas.")),
            };
            artifact.source_revisions = serde_json::from_str(&sources).map_err(invalid)?;
            artifacts.push(artifact);
        }
        let bases=db.prepare("SELECT a.relative_path,a.content_sha256 FROM artifacts a LEFT JOIN artifact_publications o ON o.artifact_id=a.id JOIN pulsations owner ON owner.id=a.pulsation_id WHERE owner.project_id=?1 AND a.publication_state='applied' AND a.relative_path IS NOT NULL ORDER BY COALESCE(o.updated_at,a.updated_at) DESC,a.id DESC")?.query_map([&fence.project_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
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
        let handoff = StageHandoffV1 {
            schema_version: SchemaV1,
            recipient: fence.clone(),
            stage,
            assessment: None,
            context: None,
            answers: Vec::new(),
            artifacts,
            publication_bases,
            revision_feedback: db.query_row(
                "SELECT revision_feedback FROM pulsations WHERE id=?1",
                [pulse],
                |r| r.get(0),
            )?,
        };
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
        let outcome = execution.outcome()?;
        outcome.validate_for(handoff.stage).map_err(invalid)?;
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
            .ok_or_else(|| invalid("Sem pulsação"))?;
        let current =
            Self::find_pulsation_by_id(&tx, pulse)?.ok_or_else(|| invalid("Pulsação ausente"))?;
        if current.status != PulsationStatus::Running || current.stage != handoff.stage {
            return Err(invalid(ContractError::StaleExecution));
        }
        Self::update_agent_run_metrics(&tx, run_id, execution)?;
        Self::persist_agent_run_output(&tx, run_id, job, execution)?;
        let now = Self::now_iso();
        tx.execute("INSERT INTO stage_acceptances(job_id,generation,agent_run_id,fence_json,outcome_json,handoff_json,accepted_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![job.id,handoff.recipient.generation,run_id,serde_json::to_string(&handoff.recipient).map_err(invalid)?,serde_json::to_string(&outcome).map_err(invalid)?,serde_json::to_string(handoff).map_err(invalid)?,now])?;
        let disposition = stage_disposition(current.stage, &outcome).map_err(invalid)?;
        if let StageOutcome::Completed { artifacts, .. } = &outcome {
            let mut paths = HashSet::new();
            for (position, artifact) in artifacts.iter().enumerate() {
                if artifact.content.len() > 1024 * 1024 || artifact.content.as_bytes().contains(&0)
                {
                    return Err(invalid("Artifact acima de 1 MiB ou binário."));
                }
                let path = artifact
                    .relative_path
                    .as_ref()
                    .map(RelativeSourcePath::as_str);
                if let Some(path) = path {
                    publication::safe_target(root, path, false)?;
                    if !paths.insert(path.to_ascii_lowercase()) {
                        return Err(invalid("Artifacts têm destinos duplicados."));
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
                    // Only overwrite an earlier host publication observed in THIS handoff.
                    // A newly suggested path must be absent, never blindly sampled after the LLM call.
                    let expected = handoff
                        .publication_bases
                        .iter()
                        .find(|base| base.relative_path.as_str().eq_ignore_ascii_case(path))
                        .map(|base| base.content_sha256.as_str());
                    tx.execute("INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,expected_sha256,desired_sha256,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,current.project_id,pulse,path,expected,hash.as_str(),now])?;
                }
            }
        }
        match disposition {
            StageDisposition::AwaitArtifactReview => {
                let updated = Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Checkpoint,
                    current.stage,
                    "checkpoint_ready",
                    Some("Checkpoint pronto para revisão HITL."),
                )?;
                Self::open_checkpoint(&tx, &updated.id, updated.stage)?;
                tx.execute("UPDATE checkpoints SET state_version=(SELECT state_version FROM pulsations WHERE id=?1),input_revision=?2,producer_job_id=?3,generation=?4 WHERE pulsation_id=?1 AND status='waiting'",params![pulse,handoff.recipient.input_revision,job.id,handoff.recipient.generation])?;
            }
            StageDisposition::Advance(next) => {
                let updated = Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Planning,
                    next,
                    "stage_auto_advanced",
                    Some("Resultado aceito; aguardando publicações obrigatórias."),
                )?;
                Self::enqueue_stage_job(&tx, &updated.id, updated.stage)?;
            }
            StageDisposition::Fail => {
                Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Failed,
                    current.stage,
                    "stage_failed",
                    Some("O agente reportou falha; nenhum artifact foi publicado."),
                )?;
                tx.execute(
                    "UPDATE pulsations SET terminal_reason='model_reported_failure' WHERE id=?1",
                    [pulse],
                )?;
                Self::mark_effects_for_review(&tx, pulse, "stage_failed")?;
            }
            StageDisposition::AwaitClarification => {
                return Err(invalid(
                    "Esclarecimentos só serão ativados com a experiência M3.",
                ));
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
        tx.commit()?;
        Ok(())
    }

    pub(super) fn mark_effects_for_review(
        db: &Connection,
        pulse: &str,
        reason: &str,
    ) -> Result<(), PersistenceServiceError> {
        db.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason=?2,state_version=state_version+1 WHERE project_id=(SELECT project_id FROM pulsations WHERE id=?1) AND EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND publication_state IN ('applied','applying','conflict'))",params![pulse,reason])?;
        Ok(())
    }

    /// Replays accepted intents only. Each unit holds an IMMEDIATE SQLite transaction
    /// across replace+ACK, serializing user cancellation without holding a lock over await.
    pub(super) fn reconcile_publications(&self) -> Result<(), PersistenceServiceError> {
        loop {
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate=tx.query_row("SELECT o.artifact_id FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id JOIN pulsations p ON p.id=o.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE j.generation=a.generation AND j.status='done' AND o.state IN ('pending','applying') AND a.acceptance='accepted' AND a.input_revision=p.input_revision AND p.status<>'failed' ORDER BY o.updated_at,o.artifact_id LIMIT 1",[],|r|r.get::<_,String>(0)).optional()?;
            let Some(id) = candidate else {
                tx.commit()?;
                break;
            };
            tx.execute(
                "UPDATE artifact_publications SET state='applying' WHERE artifact_id=?1",
                [&id],
            )?;
            tx.execute(
                "UPDATE artifacts SET publication_state='applying' WHERE id=?1",
                [&id],
            )?;
            tx.commit()?;
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
        self.complete_serial_integrations()?;
        Ok(())
    }

    fn apply_publication_unit(&self, id: &str) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row=tx.query_row("SELECT p.workspace_path,o.relative_path,o.expected_sha256,a.content,o.pulsation_id,o.desired_sha256,a.content_sha256 FROM artifact_publications o JOIN artifacts a ON a.id=o.artifact_id JOIN projects p ON p.id=o.project_id JOIN pulsations u ON u.id=o.pulsation_id JOIN jobs j ON j.id=a.producer_job_id WHERE j.generation=a.generation AND j.status='done' AND o.artifact_id=?1 AND o.state='applying' AND a.acceptance='accepted' AND a.input_revision=u.input_revision AND u.status<>'failed'",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?))).optional()?;
        let Some((root, path, expected, content, pulse, desired, artifact_hash)) = row else {
            tx.commit()?;
            return Ok(());
        };
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
                "Hash/conteúdo durável inconsistente; publicação recusada.",
            ))
        } else {
            publication::publish(Path::new(&root), &path, &expected, &content)
        };
        match result {
            Ok(()) => {
                tx.execute("UPDATE artifact_publications SET state='applied',last_error=NULL,updated_at=?2 WHERE artifact_id=?1",params![id,Self::now_iso()])?;
                tx.execute(
                    "UPDATE artifacts SET publication_state='applied' WHERE id=?1",
                    [id],
                )?;
            }
            Err(error) => {
                tx.execute("UPDATE artifact_publications SET state='conflict',last_error=?2,updated_at=?3 WHERE artifact_id=?1",params![id,error.to_string(),Self::now_iso()])?;
                tx.execute(
                    "UPDATE artifacts SET publication_state='conflict' WHERE id=?1",
                    [id],
                )?;
                let current = Self::find_pulsation_by_id(&tx, &pulse)?
                    .ok_or_else(|| invalid("Pulsação ausente"))?;
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

    fn complete_serial_integrations(&self) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids=tx.prepare("SELECT id FROM pulsations p WHERE p.stage='Integrator' AND p.status='running' AND EXISTS(SELECT 1 FROM checkpoints c WHERE c.pulsation_id=p.id AND c.stage='Integrator' AND c.status='approved' AND c.input_revision=p.input_revision) AND NOT EXISTS(SELECT 1 FROM artifact_publications o WHERE o.pulsation_id=p.id AND o.state NOT IN ('applied','cancelled'))")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        for id in ids {
            let current =
                Self::find_pulsation_by_id(&tx, &id)?.ok_or_else(|| invalid("Pulsação ausente"))?;
            Self::transition_pulsation(
                &tx,
                &current,
                PulsationStatus::Done,
                current.stage,
                "hitl_approved_final",
                Some("Integração aprovada e publicações reconciliadas."),
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
