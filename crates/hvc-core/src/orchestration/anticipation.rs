//! One background slot: immediate successor Intake, then local PreSpec.
use super::*;
use crate::orchestration::*;
use coordination::invalid;
use rusqlite::TransactionBehavior;

impl PersistenceService {
    pub(super) fn claim_anticipation(
        &self,
    ) -> Result<Option<(Job, CancellationToken)>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !Self::preparation_settings(&tx)?.preparation_enabled {
            return Ok(None);
        }
        // A claim never starts a second collector for the same wait/revision.
        let pulse:Option<String>=tx.query_row("SELECT p.id FROM pulsations p JOIN pulsation_preparation r ON r.pulsation_id=p.id WHERE p.id IN (SELECT id FROM anticipatable_pulsations) AND r.review_state!='pending' AND p.stage='SPECIFICATION' AND p.specification_started=0 AND r.prespec_state='not_scheduled' ORDER BY p.created_at,p.project_id,p.queue_sequence,p.id LIMIT 1",[],|r|r.get(0)).optional()?;
        if let Some(pulse) = pulse {
            let assessment: String = tx.query_row(
                "SELECT assessment_json FROM pulsation_preparation WHERE pulsation_id=?1",
                [&pulse],
                |r| r.get(0),
            )?;
            let assessment: VaguenessAssessment =
                serde_json::from_str(&assessment).map_err(|e| invalid(e.to_string()))?;
            if assessment.score.get() >= assessment.threshold.get() {
                tx.execute("UPDATE pulsation_preparation SET prespec_state='skipped',skip_reason='ambiguous_request' WHERE pulsation_id=?1",[pulse])?;
            } else {
                let job = Uuid::new_v4().to_string();
                tx.execute("INSERT INTO jobs(id,pulsation_id,kind,status,attempts,created_at,updated_at,project_id,input_revision,state_version,job_class,idempotency_key) SELECT ?2,p.id,'prepare_context','pending',0,?3,?3,p.project_id,p.input_revision,p.state_version,'background',p.id||':'||p.input_revision||':prepare_context' FROM pulsations p WHERE p.id=?1",params![pulse,job,Self::now_iso()])?;
                tx.execute("UPDATE pulsation_preparation SET job_id=?2,prespec_state='pending',predecessor_id=(SELECT s.active_pulsation_id FROM project_pipeline_state s JOIN pulsations p ON p.project_id=s.project_id WHERE p.id=?1) WHERE pulsation_id=?1",params![pulse,job])?;
            }
        }
        let job=tx.query_row("SELECT j.id,j.pulsation_id,j.kind,j.status,j.attempts,j.last_error,j.created_at,j.updated_at,j.started_at,j.finished_at,j.cancel_requested_at,j.available_at FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id JOIN pulsation_preparation r ON r.pulsation_id=p.id WHERE p.id IN (SELECT id FROM anticipatable_pulsations) AND j.status='pending' AND j.cancellation_reason IS NULL AND j.input_revision=p.input_revision AND j.state_version=p.state_version AND r.review_state!='pending' AND (j.kind='run_stage::Intake' AND p.stage='Intake' OR j.kind='prepare_context' AND p.stage='SPECIFICATION' AND r.prespec_state='pending') AND (j.available_at IS NULL OR j.available_at<=?1) AND NOT EXISTS(SELECT 1 FROM runtime_slots WHERE job_class='background' OR job_id=j.id) ORDER BY CASE j.kind WHEN 'run_stage::Intake' THEN 0 ELSE 1 END,p.created_at,p.project_id,p.queue_sequence,j.id LIMIT 1",[Self::now_iso()],Self::map_job).optional()?;
        let Some(mut job) = job else {
            tx.commit()?;
            return Ok(None);
        };
        let pulse = job.pulsation_id.as_deref().expect("pulse job");
        if self.user_cancellation_pending(pulse) {
            tx.commit()?;
            return Ok(None);
        }
        let project: String = tx.query_row(
            "SELECT project_id FROM pulsations WHERE id=?1",
            [pulse],
            |r| r.get(0),
        )?;
        job.attempts += 1;
        if !Self::reserve_slot(
            &tx,
            JobClass::Background,
            &Self::job_slot_token(&job),
            Some(&project),
            Some(&job.id),
        )? {
            tx.commit()?;
            return Ok(None);
        }
        tx.execute("UPDATE jobs SET status='running',job_class='background',generation=generation+1,attempts=attempts+1,started_at=?2,updated_at=?2 WHERE id=?1",params![job.id,Self::now_iso()])?;
        if job.kind == "prepare_context" {
            tx.execute("UPDATE pulsation_preparation SET prespec_state='running',updated_at=?2 WHERE pulsation_id=?1",params![pulse,Self::now_iso()])?;
        }
        let cancellation = CancellationToken::new();
        self.register_job_cancellation(&job.id, cancellation.clone());
        if let Err(error) = tx.commit() {
            self.unregister_job_cancellation(&job.id);
            return Err(error.into());
        }
        job.status = JobStatus::Running;
        Ok(Some((job, cancellation)))
    }

    pub(super) async fn process_anticipation(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<bool, PersistenceServiceError> {
        let service = self.clone();
        let claim = tokio::task::spawn_blocking(move || service.claim_anticipation())
            .await
            .map_err(|e| invalid(e.to_string()))??;
        let Some((job, cancellation)) = claim else {
            return Ok(false);
        };
        let _slot = self.slot_guard(Self::job_slot_token(&job));
        if shutdown.is_cancelled() {
            cancellation.cancel();
        }
        let result = if job.kind == "prepare_context" {
            let service = self.clone();
            let work = job.clone();
            let stop = cancellation.clone();
            // If shutdown arrives during the blocking read, request cancellation and JOIN it.
            let task =
                tokio::task::spawn_blocking(move || service.execute_preparation(&work, &stop));
            tokio::pin!(task);
            tokio::select! {
                result=&mut task=>result.map_err(|e|invalid(e.to_string()))?,
                _=shutdown.cancelled()=>{ cancellation.cancel(); task.await.map_err(|e|invalid(e.to_string()))? }
            }
        } else {
            self.process_claimed_job(&job, &cancellation).await
        };
        self.unregister_job_cancellation(&job.id);
        if let Err(error) = &result {
            if job.kind == "prepare_context" {
                self.connection()?.execute("UPDATE jobs SET status='failed',last_error=?2,finished_at=?3,updated_at=?3 WHERE id=?1 AND status='running'",params![job.id,error.to_string(),Self::now_iso()])?;
                self.connection()?.execute("UPDATE pulsation_preparation SET prespec_state='failed',diagnostic=?2 WHERE job_id=?1 AND prespec_state='running'",params![job.id,error.to_string()])?;
            } else {
                self.close_abandoned_claim(&job, &error.to_string())?;
            }
        }
        result.map(|_| true)
    }

    pub(super) fn execute_preparation(
        &self,
        job: &Job,
        cancellation: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let db = self.connection()?;
        let fence = Self::execution_fence(&db, &job.id)?;
        let (request, epoch) = Self::context_request(&db, &fence)?;
        let started = std::time::Instant::now();
        let collected = RoutedCodeContextService::collect_blocking(&request, cancellation);
        let facts = match collected {
            Ok(facts) if !cancellation.is_cancelled() => {
                let remaining = std::time::Duration::from_millis(request.budget.max_duration_ms)
                    .saturating_sub(started.elapsed());
                self.expand_context_through_brownfield_graph(
                    &request,
                    facts,
                    cancellation,
                    remaining,
                )
                .map(|(facts, _)| facts)
            }
            Ok(facts) => Ok(facts),
            Err(error) => Err(invalid(error.to_string())),
        };
        let elapsed = started.elapsed();
        #[cfg(test)]
        if let Some((entered, release)) = self
            .runtime_control
            .preparation_gate
            .lock()
            .unwrap()
            .clone()
        {
            entered.wait();
            release.wait();
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs j JOIN pulsations p ON p.id=j.pulsation_id JOIN pulsation_preparation r ON r.job_id=j.id JOIN project_pipeline_state s ON s.project_id=p.project_id WHERE j.id=?1 AND j.status='running' AND j.cancellation_reason IS NULL AND j.generation=?2 AND j.input_revision=p.input_revision AND j.state_version=p.state_version AND r.input_revision=p.input_revision AND s.active_pulsation_id=r.predecessor_id AND s.base_epoch=?3 AND p.id IN (SELECT id FROM anticipatable_pulsations))",params![job.id,fence.generation,epoch],|r|r.get(0))?;
        if !valid || cancellation.is_cancelled() {
            // A late result must never cancel preparation created for a newer input revision.
            tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',finished_at=?2,updated_at=?2 WHERE id=?1 AND generation=?3 AND status='running'",params![job.id,Self::now_iso(),fence.generation])?;
            tx.execute("UPDATE pulsation_preparation SET prespec_state='cancelled',skip_reason='base_released',updated_at=?3 WHERE job_id=?1 AND input_revision=?2 AND prespec_state IN ('pending','running')",params![job.id,fence.input_revision,Self::now_iso()])?;
            tx.commit()?;
            return Ok(());
        }
        let facts = facts?;
        Self::record_context_collection(&tx, &fence, "preparation", "prepared", elapsed, &facts)?;
        let package = ContextPackageV1 {
            schema_version: SchemaV1,
            pulsation_id: fence.pulsation_id.clone().expect("pulse"),
            input_revision: fence.input_revision.expect("revision"),
            ruleset_version: task_refine::ruleset_version().into(),
            observed_base_epoch: epoch,
            budget: request.budget,
            facts,
            created_at: Self::now_iso(),
        };
        Self::record_brownfield_usage_tx(&tx, &fence.project_id, &package.facts.sources)?;
        let content = serde_json::to_string(&package).map_err(|e| invalid(e.to_string()))?;
        let artifact = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO artifacts(id,pulsation_id,name,kind,content_sha256,created_at,updated_at,content,producer_job_id,producer_stage,generation,input_revision,producer_state_version,output_position,acceptance,publication_state) VALUES(?1,?2,'Contexto PreSpec','code_context',?3,?4,?4,?5,?6,'Intake',?7,?8,?9,0,'accepted','not_required')",params![artifact,package.pulsation_id,ContentSha256::of(content.as_bytes()).as_str(),Self::now_iso(),content,job.id,fence.generation,fence.input_revision,fence.state_version])?;
        tx.execute("UPDATE pulsation_preparation SET prespec_state='ready',context_artifact_id=?2,skip_reason=?3,updated_at=?4 WHERE job_id=?1",params![job.id,artifact,if package.facts.sources.is_empty(){Some("no_matches")}else{None},Self::now_iso()])?;
        tx.execute(
            "UPDATE jobs SET status='done',finished_at=?2,updated_at=?2 WHERE id=?1",
            params![job.id, Self::now_iso()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn context_request(
        db: &Connection,
        fence: &ExecutionFence,
    ) -> Result<(CodeContextRequest, u64), PersistenceServiceError> {
        let (root,query,epoch):(String,String,u64)=db.query_row("SELECT project.workspace_path,p.feature_name,s.base_epoch FROM pulsations p JOIN projects project ON project.id=p.project_id JOIN project_pipeline_state s ON s.project_id=p.project_id WHERE p.id=?1",[fence.pulsation_id.as_deref().expect("pulse")],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        Ok((
            CodeContextRequest {
                project_id: fence.project_id.clone(),
                workspace_root: PathBuf::from(root),
                query,
                hinted_paths: vec![],
                budget: CollectionBudget::default(),
            },
            epoch,
        ))
    }

    pub(super) async fn enrich_specification(
        &self,
        mut handoff: StageHandoffV1,
        budget: usize,
        cancellation: &CancellationToken,
    ) -> Result<StageHandoffV1, PersistenceServiceError> {
        let service = self.clone();
        let stop = cancellation.clone();
        tokio::task::spawn_blocking(move|| {
            let db=service.connection()?;
            let pulse=handoff.recipient.pulsation_id.as_deref().expect("pulse");
            let (request,epoch)=Self::context_request(&db,&handoff.recipient)?;
            let prepared:Option<(String,String)>=db.query_row("SELECT a.content,a.content_sha256 FROM pulsation_preparation r JOIN artifacts a ON a.id=r.context_artifact_id JOIN jobs j ON j.id=a.producer_job_id WHERE r.pulsation_id=?1 AND r.input_revision=?2 AND r.prespec_state='ready' AND a.acceptance='accepted' AND a.kind='code_context' AND a.relative_path IS NULL AND a.content IS NOT NULL AND a.content_sha256 IS NOT NULL AND a.generation=j.generation AND a.input_revision=r.input_revision AND j.status='done' AND r.consumed_job_id IS NULL",params![pulse,handoff.recipient.input_revision],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let old=prepared.as_ref().filter(|(s,hash)|ContentSha256::of(s.as_bytes()).as_str()==hash).and_then(|(s,_)|serde_json::from_str::<ContextPackageV1>(s).ok());
            // Full bounded rediscovery detects additions/deletions/renames and changed match sets.
            let compatible=old.as_ref().filter(|old| old.pulsation_id==pulse && Some(old.input_revision)==handoff.recipient.input_revision && old.ruleset_version==task_refine::ruleset_version() && old.budget==request.budget);
            let started=std::time::Instant::now();
            let (facts,reusable)=RoutedCodeContextService::collect_revalidated(&request,&stop,compatible.map(|p|&p.facts)).map_err(|e|invalid(e.to_string()))?;
            let remaining=std::time::Duration::from_millis(request.budget.max_duration_ms).saturating_sub(started.elapsed());
            let (facts,graph_applied)=service.expand_context_through_brownfield_graph(&request,facts,&stop,remaining)?;
            let reusable=reusable && !graph_applied;
            let elapsed=started.elapsed();
            let measured_facts=facts.clone();
            let disposition=if reusable {"reused"} else if prepared.is_some(){"stale_on_demand"}else{"on_demand"};
            let mut package=if reusable {old.expect("reusable")}else{ContextPackageV1{schema_version:SchemaV1,pulsation_id:pulse.into(),input_revision:handoff.recipient.input_revision.expect("revision"),ruleset_version:task_refine::ruleset_version().into(),observed_base_epoch:epoch,budget:request.budget,facts,created_at:Self::now_iso()}};
            // Only optional lexical evidence is trimmed. Approved artifacts/answers remain whole.
            let normative=handoff.normative_chars();
            let remaining=budget.saturating_sub(normative);
            while serde_json::to_string(&package).map_err(|e|invalid(e.to_string()))?.chars().count()>remaining && !package.facts.sources.is_empty() {
                let removed=package.facts.sources.pop().expect("source");
                package.facts.snippets.retain(|s|s.source!=removed);
                package.facts.symbols.retain(|s|s.source!=removed);
                package.facts.references.retain(|r|r.source!=removed);
                package.facts.coverage.truncated=true; package.facts.coverage.state=CoverageState::Partial;
            }
            handoff.context=Some(package);
            handoff.validate_for(&handoff.recipient,budget).map_err(|e|invalid(e.to_string()))?;
            let mut db=service.connection()?;
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Self::check_execution(&tx,&handoff.recipient)?;
            if let Some(context)=&handoff.context {
                Self::record_brownfield_usage_tx(&tx,&handoff.recipient.project_id,&context.facts.sources)?;
            }
            Self::record_context_collection(&tx,&handoff.recipient,"specification",disposition,elapsed,&measured_facts)?;
            let diagnostic=handoff.context.as_ref().map(|c|format!("Coleta/revalidação no epoch {epoch}, {}. {}",Self::now_iso(),c.facts.coverage.diagnostics.join(" ")));
            tx.execute("UPDATE pulsation_preparation SET consumed_job_id=?3,context_disposition=?4,diagnostic=?5,prespec_state=CASE WHEN ?4='stale_on_demand' THEN 'stale' ELSE prespec_state END WHERE pulsation_id=?1 AND input_revision=?2",params![pulse,handoff.recipient.input_revision,handoff.recipient.job_id,disposition,diagnostic])?;
            tx.commit()?;
            Ok(handoff)
        }).await.map_err(|e|invalid(e.to_string()))?
    }

    /// Operational counters only: never source paths, snippets, queries or diagnostics.
    /// Written with the fenced acceptance, not for a cancelled/obsolete collection.
    fn record_context_collection(
        db: &Connection,
        fence: &ExecutionFence,
        phase: &str,
        disposition: &str,
        elapsed: std::time::Duration,
        facts: &CodeContextFacts,
    ) -> Result<(), PersistenceServiceError> {
        let metrics = serde_json::json!({
            "schema_version": 1,
            "job_id": fence.job_id,
            "generation": fence.generation,
            "input_revision": fence.input_revision,
            "phase": phase,
            "disposition": disposition,
            "elapsed_us": elapsed.as_micros().min(u128::from(u64::MAX)) as u64,
            "enumerated_files": facts.coverage.enumerated_files,
            "read_bytes": facts.coverage.read_bytes,
            "selected_files": facts.sources.len(),
            "coverage": facts.coverage.state,
            "truncated": facts.coverage.truncated,
        });
        db.execute("INSERT INTO pulse_events(id,pulsation_id,event_type,message,created_at) VALUES(?1,?2,'context_collected',?3,?4)", params![Uuid::new_v4().to_string(),fence.pulsation_id,metrics.to_string(),Self::now_iso()])?;
        Ok(())
    }
}
