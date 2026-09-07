//! Integration barrier: approval + current filesystem evidence + one final transaction.
use super::*;
use crate::orchestration::{ContentSha256, SourceRevision, publication};

impl PersistenceService {
    pub(super) fn complete_integrations(&self) -> Result<bool, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ids = tx.prepare(
            "SELECT p.id FROM pulsations p JOIN project_pipeline_state s ON s.active_pulsation_id=p.id
             JOIN checkpoints c ON c.id=p.integration_checkpoint_id
             WHERE p.stage='Integrator' AND p.status='running' AND p.coordination_hold=0
               AND s.base_state<>'needs_review' AND p.integration_state_version=p.state_version
               AND c.status='approved' AND c.purpose='artifact_review' AND c.stage='Integrator'
               AND c.pulsation_id=p.id AND c.input_revision=p.input_revision
               AND ((p.pipeline_version=1 AND c.producer_job_id IS NULL) OR EXISTS(
                   SELECT 1 FROM jobs j WHERE j.id=c.producer_job_id AND j.generation=c.generation AND j.status='done'))
               AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.pulsation_id=p.id AND j.status IN ('pending','running','interrupted'))
               AND NOT EXISTS(SELECT 1 FROM artifact_publications o WHERE o.pulsation_id=p.id AND o.state NOT IN ('applied','cancelled'))
             ORDER BY p.project_id,p.queue_sequence",
        )?.query_map([], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut progressed = false;
        for id in ids {
            if self.user_cancellation_pending(&id) {
                continue;
            }
            progressed = true;
            let current = Self::find_pulsation_by_id(&tx, &id)?
                .ok_or_else(|| coordination::invalid("Pulsação ausente"))?;
            if let Err(error) = Self::verify_integration_files(&tx, &id) {
                Self::transition_pulsation(
                    &tx,
                    &current,
                    PulsationStatus::Failed,
                    current.stage,
                    "integration_verification_conflict",
                    Some(&error.to_string()),
                )?;
                tx.execute(
                    "UPDATE pulsations SET terminal_reason='publication_conflict' WHERE id=?1",
                    [&id],
                )?;
                tx.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason='integration_verification_conflict',state_version=state_version+1 WHERE active_pulsation_id=?1", [&id])?;
                Self::enqueue_learning(&tx, &id)?;
                continue;
            }
            crate::feature_forge::pipeline::complete_batch(&tx, &id)?;
            Self::transition_pulsation(
                &tx,
                &current,
                PulsationStatus::Done,
                current.stage,
                "hitl_approved_final",
                Some(
                    "Integração aprovada; hashes atuais conferidos (ou no-op sem arquivos). Base liberada; isto não certifica execução de testes/Git.",
                ),
            )?;
            tx.execute("UPDATE project_pipeline_state SET base_epoch=base_epoch+1,active_pulsation_id=NULL,base_state='ready',blocking_reason=NULL,state_version=state_version+1 WHERE active_pulsation_id=?1", [&id])?;
            Self::enqueue_learning(&tx, &id)?;
            Self::block_held_successors(&tx)?;
        }
        tx.commit()?;
        Ok(progressed)
    }

    fn verify_integration_files(
        db: &Connection,
        pulse: &str,
    ) -> Result<(), PersistenceServiceError> {
        let (project, root): (String,String) = db.query_row("SELECT p.id,p.workspace_path FROM projects p JOIN pulsations u ON u.project_id=p.id WHERE u.id=?1", [pulse], |r| Ok((r.get(0)?,r.get(1)?)))?;
        let unresolved: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND acceptance='accepted' AND (legacy_adaptation='unavailable_or_ambiguous' OR (relative_path IS NOT NULL AND publication_state<>'applied')))", [pulse], |r| r.get(0))?;
        if unresolved {
            return Err(coordination::invalid(
                "Artefatos obrigatórios ainda não verificados.",
            ));
        }
        let missing_baseline: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM artifacts a WHERE a.pulsation_id=?1 AND a.publication_state='applied' AND a.relative_path IS NOT NULL AND NOT EXISTS(SELECT 1 FROM project_file_baselines b WHERE b.project_id=?2 AND b.relative_path=a.relative_path COLLATE NOCASE))",params![pulse,project],|r|r.get(0))?;
        if missing_baseline {
            return Err(coordination::invalid(
                "Baseline publicada indisponível; reconciliação necessária.",
            ));
        }
        // Later stages may replace the same path. Check the final host baseline, not each older draft.
        let paths = db.prepare("SELECT DISTINCT a.relative_path,b.content_sha256 FROM artifacts a JOIN project_file_baselines b ON b.project_id=?2 AND b.relative_path=a.relative_path COLLATE NOCASE WHERE a.pulsation_id=?1 AND a.publication_state='applied'")?
            .query_map(params![pulse,project], |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (path, hash) in paths {
            let observed = publication::observed_hash(Path::new(&root), &path)?;
            if observed.as_ref().map(ContentSha256::as_str) != hash.as_deref() {
                return Err(coordination::invalid(format!(
                    "Arquivo mudou antes da liberação da base: {path}"
                )));
            }
        }
        let sources = db.prepare("SELECT source_revisions_json FROM artifacts WHERE pulsation_id=?1 AND acceptance='accepted'")?
            .query_map([pulse], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        for json in sources {
            let revisions: Vec<SourceRevision> =
                serde_json::from_str(&json).map_err(|e| coordination::invalid(e.to_string()))?;
            for source in revisions {
                if source.project_id != project
                    || publication::observed_hash(Path::new(&root), source.relative_path.as_str())?
                        .as_ref()
                        != Some(&source.content_sha256)
                {
                    return Err(coordination::invalid(
                        "Fonte de nota obrigatória mudou; é necessária nova revisão.",
                    ));
                }
            }
        }
        Ok(())
    }
}
