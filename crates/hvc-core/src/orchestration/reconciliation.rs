//! Explicit, versioned adoption of a reviewed filesystem base. Never retries/overwrites conflicts.
use super::*;
use crate::orchestration::{
    BaseState, ConfirmBaseReconciliationRequest, ContentSha256, ProjectCoordinationSnapshot,
    ProjectPipelineState, PublicationReview, QueuePosition, publication,
};

impl PersistenceService {
    pub fn get_project_coordination(
        &self,
        project: &str,
    ) -> Result<ProjectCoordinationSnapshot, PersistenceServiceError> {
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        let result = Self::coordination_snapshot(&tx, project)?;
        tx.commit()?;
        Ok(result)
    }

    pub(super) fn coordination_snapshot(
        db: &Connection,
        project: &str,
    ) -> Result<ProjectCoordinationSnapshot, PersistenceServiceError> {
        let pipeline = db.query_row("SELECT active_pulsation_id,base_epoch,base_state,state_version,blocking_reason FROM project_pipeline_state WHERE project_id=?1", [project], |r| {
            let state: String = r.get(2)?;
            Ok(ProjectPipelineState { project_id: project.into(), active_pulsation_id:r.get(0)?, base_epoch:r.get(1)?,
                base_state:match state.as_str() { "ready"=>BaseState::Ready,"publishing"=>BaseState::Publishing,_=>BaseState::NeedsReview }, version:r.get(3)?,blocking_reason:r.get(4)? })
        })?;
        let queue = db.prepare("SELECT p.id,p.queue_sequence,p.status,p.coordination_hold,p.id IN (SELECT id FROM coordinated_pulsations) FROM pulsations p WHERE p.project_id=?1 ORDER BY p.queue_sequence")?
            .query_map([project], |r| {
                let status:String=r.get(2)?;
                let held:bool=r.get(3)?;
                let admitted:bool=r.get(4)?;
                let reason=if matches!(status.as_str(),"done"|"failed") {None}
                    else if pipeline.base_state==BaseState::NeedsReview {Some("needs_review")}
                    else if held {Some("legacy_owner_selection")}
                    else if !admitted {Some("waiting_for_base")}
                    else if status=="checkpoint" {Some("waiting_for_user")}
                    else if pipeline.base_state==BaseState::Publishing {Some("publishing")}
                    else {None};
                Ok(QueuePosition {pulsation_id:r.get(0)?,sequence:r.get(1)?,blocking_reason:reason.map(str::to_owned)})
            })?.collect::<Result<Vec<_>,_>>()?;
        let owner_candidates = db.prepare("SELECT p.id FROM pulsations p JOIN project_pipeline_state s ON s.project_id=p.project_id WHERE p.project_id=?1 AND p.stage<>'Intake' AND p.status NOT IN ('done','failed') AND (p.coordination_hold=1 OR s.active_pulsation_id=p.id OR EXISTS(SELECT 1 FROM jobs j WHERE j.pulsation_id=p.id AND j.kind<>'run_stage::Intake' AND j.attempts>0)) ORDER BY p.queue_sequence")?
            .query_map([project], |r| r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let root: String = db.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [project],
            |r| r.get(0),
        )?;
        let mut publications = db.prepare("SELECT a.id,a.pulsation_id,a.relative_path,a.publication_state,a.content_sha256,o.last_error FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id LEFT JOIN artifact_publications o ON o.artifact_id=a.id WHERE p.project_id=?1 AND p.status<>'done' AND a.reconciled_at IS NULL AND a.relative_path IS NOT NULL AND a.publication_state IN ('applied','applying','conflict') ORDER BY a.id")?
            .query_map([project], |r| Ok(PublicationReview {artifact_id:r.get(0)?,pulsation_id:r.get(1)?,relative_path:r.get(2)?,state:r.get(3)?,desired_sha256:r.get(4)?,observed_sha256:None,diagnostic:r.get(5)?,observation_error:None}))?.collect::<Result<Vec<_>,_>>()?;
        if pipeline.base_state != BaseState::NeedsReview {
            publications.clear();
        }
        for item in &mut publications {
            match publication::observed_hash(Path::new(&root), &item.relative_path) {
                Ok(hash) => item.observed_sha256 = hash.map(|v| v.as_str().to_owned()),
                Err(error) => item.observation_error = Some(error.to_string()),
            }
        }
        let bytes = serde_json::to_vec(&(&pipeline, &queue, &owner_candidates, &publications))
            .map_err(|e| coordination::invalid(e.to_string()))?;
        let review_token = ContentSha256::of(&bytes).as_str().to_owned();
        Ok(ProjectCoordinationSnapshot {
            pipeline,
            queue,
            owner_candidates,
            publications,
            review_token,
        })
    }

    pub fn confirm_base_reconciliation(
        &self,
        request: ConfirmBaseReconciliationRequest,
    ) -> Result<ProjectCoordinationSnapshot, PersistenceServiceError> {
        let explanation = request.explanation.trim();
        if explanation.is_empty() || explanation.chars().count() > 16_000 {
            return Err(coordination::invalid(
                "Describe the manual review of the base (1–16,000 characters).",
            ));
        }
        let mut db = self.connection()?;
        // The same write coordinator serializes cancellation, file units and this confirmation.
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = Self::coordination_snapshot(&tx, &request.project_id)?;
        if current.pipeline.base_state != BaseState::NeedsReview
            || current.pipeline.base_epoch != request.expected_base_epoch
            || current.pipeline.version != request.expected_pipeline_state_version
            || current.review_token != request.expected_review_token
        {
            return Err(coordination::invalid(
                "The base or files changed since the review. Refresh and check again.",
            ));
        }
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runtime_slots WHERE project_id=?1) OR EXISTS(SELECT 1 FROM jobs WHERE project_id=?1 AND status='running')",[&request.project_id],|r|r.get(0))?;
        if busy {
            return Err(coordination::invalid(
                "Wait for the running or publishing unit to finish before reconciling.",
            ));
        }
        match &request.resume_pulsation_id {
            Some(id) if current.owner_candidates.contains(id) => {
                let unavailable:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE pulsation_id=?1 AND legacy_adaptation='unavailable_or_ambiguous')",[id],|r|r.get(0))?;
                let mut divergent = unavailable;
                let mut seen = HashSet::new();
                for item in current
                    .publications
                    .iter()
                    .filter(|p| &p.pulsation_id == id)
                {
                    if !seen.insert(item.relative_path.to_ascii_lowercase()) {
                        continue;
                    }
                    let expected: Option<String> = tx.query_row("SELECT a.content_sha256 FROM artifacts a LEFT JOIN artifact_publications o ON o.artifact_id=a.id WHERE a.pulsation_id=?1 AND a.relative_path=?2 COLLATE NOCASE AND a.publication_state IN ('applied','applying','conflict') ORDER BY COALESCE(o.updated_at,a.updated_at) DESC,a.rowid DESC LIMIT 1",params![id,item.relative_path],|r|r.get(0))?;
                    divergent |=
                        item.observation_error.is_some() || item.observed_sha256 != expected;
                }
                if divergent {
                    return Err(coordination::invalid(
                        "The selected pulsation has missing or diverging evidence. Cancel it before adopting the current base; no output will be discarded.",
                    ));
                }
            }
            None if current.owner_candidates.is_empty() => {}
            _ => {
                return Err(coordination::invalid(
                    "Choose which advanced pulsation resumes. The others are kept.",
                ));
            }
        }
        let now = Self::now_iso();
        for item in &current.publications {
            // A protected/unreadable path is never adopted as a writable baseline.
            let root: String = tx.query_row(
                "SELECT workspace_path FROM projects WHERE id=?1",
                [&request.project_id],
                |r| r.get(0),
            )?;
            match publication::observed_hash(Path::new(&root), &item.relative_path) {
                Ok(observed) if item.observation_error.is_none() => {
                    if observed.as_ref().map(ContentSha256::as_str)
                        != item.observed_sha256.as_deref()
                    {
                        return Err(coordination::invalid(
                            "A file changed during the confirmation; review again.",
                        ));
                    }
                    tx.execute("INSERT INTO project_file_baselines(project_id,relative_path,content_sha256,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(project_id,relative_path) DO UPDATE SET content_sha256=excluded.content_sha256,updated_at=excluded.updated_at",params![request.project_id,item.relative_path,item.observed_sha256,now])?;
                }
                Err(error)
                    if item.observation_error.as_deref() == Some(error.to_string().as_str()) => {}
                _ => {
                    return Err(coordination::invalid(
                        "A file's availability changed during the confirmation; review again.",
                    ));
                }
            }
            tx.execute(
                "UPDATE artifacts SET reconciled_at=?2 WHERE id=?1 AND pulsation_id IN (SELECT id FROM pulsations WHERE status='failed')",
                params![item.artifact_id, now],
            )?;
        }
        tx.execute("UPDATE artifact_publications SET state='cancelled' WHERE project_id=?1 AND job_class='main' AND state IN ('pending','applying','conflict') AND pulsation_id IN (SELECT id FROM pulsations WHERE status='failed')",[&request.project_id])?;
        for id in &current.owner_candidates {
            tx.execute("UPDATE pulsations SET coordination_hold=CASE WHEN id=?2 THEN 0 ELSE 1 END WHERE id=?1",params![id,request.resume_pulsation_id])?;
        }
        tx.execute("UPDATE project_pipeline_state SET active_pulsation_id=?2,base_state='ready',base_epoch=base_epoch+1,state_version=state_version+1,blocking_reason=NULL WHERE project_id=?1",params![request.project_id,request.resume_pulsation_id])?;
        tx.execute("INSERT INTO base_reconciliations(id,project_id,previous_state_version,base_epoch,resumed_pulsation_id,explanation,observations_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![Uuid::new_v4().to_string(),request.project_id,current.pipeline.version,current.pipeline.base_epoch+1,request.resume_pulsation_id,explanation,serde_json::to_string(&current.publications).map_err(|e|coordination::invalid(e.to_string()))?,now])?;
        tx.commit()?;
        self.wake_scheduler();
        self.get_project_coordination(&request.project_id)
    }
}
