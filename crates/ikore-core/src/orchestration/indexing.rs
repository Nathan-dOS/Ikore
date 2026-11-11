//! Deterministic, coalesced note deltas. All network work uses the supervisor background slot.
use super::{
    AgentRuntimeConfig, CancellationToken, Connection, Duration, Job, JobStatus, OptionalExtension,
    Path, PathBuf, PersistenceService, PersistenceServiceError, ReindexWorkspaceEmbeddingsRequest,
    ReindexWorkspaceEmbeddingsResponse, SETTING_RUNTIME_EMBEDDING_ENABLED,
    SETTING_RUNTIME_EMBEDDING_MODEL, Uuid, coordination, fs, params,
    strip_frontmatter_for_embedding,
};
use crate::{
    orchestration::{ContentSha256, IndexingEvent, JobClass},
    workspace_service,
};
use coordination::invalid;
use rusqlite::TransactionBehavior;

pub(super) struct BackgroundClaim {
    pub job: Job,
    pub generation: u64,
    pub cancellation: CancellationToken,
}

impl PersistenceService {
    /// Vectors are derived data. Qualify them independently of the original
    /// document hash, and restart the scan after a detector-policy change.
    fn qualify_embedding_policy(
        &self,
        db: &Connection,
        root: &str,
    ) -> Result<(), PersistenceServiceError> {
        let tx = db.unchecked_transaction()?;
        let version: Option<String> = tx.query_row(
            "SELECT sanitization_policy_version FROM embedding_sync_state WHERE workspace_path=?1", [root], |row| row.get(0),
        ).optional()?;
        if version.as_deref() == Some(crate::secret_redactor::SANITIZATION_POLICY_VERSION) {
            return Ok(());
        }
        let cancelled = tx.prepare("SELECT j.id FROM jobs j JOIN embedding_deltas d ON d.job_id=j.id WHERE d.workspace_path=?1 AND j.status IN ('pending','running','interrupted')")?
            .query_map([root], |row| row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        for id in &cancelled {
            tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',updated_at=?2 WHERE id=?1", params![id,Self::now_iso()])?;
        }
        tx.execute("INSERT INTO embedding_sync_state(workspace_path,next_reconcile_at,dirty,scan_cursor,sanitization_policy_version) VALUES(?1,?2,1,'',?3) ON CONFLICT(workspace_path) DO UPDATE SET dirty=1,scan_cursor='',revision=revision+1,retry_failed=1,sanitization_policy_version=excluded.sanitization_policy_version", params![root,Self::now_iso(),crate::secret_redactor::SANITIZATION_POLICY_VERSION])?;
        tx.commit()?;
        self.signal_cancelled_jobs(&cancelled);
        Ok(())
    }
    pub(super) fn indexing_configuration_changed(
        &self,
        config: &AgentRuntimeConfig,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids=tx.prepare("SELECT j.id FROM jobs j JOIN embedding_deltas d ON d.job_id=j.id WHERE j.status IN ('pending','running','interrupted') AND (?1=0 OR d.embedding_model!=?2)")?.query_map(params![config.embedding_enabled,config.embedding_model],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        for id in &ids {
            tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',updated_at=?2 WHERE id=?1",params![id,Self::now_iso()])?;
        }
        if config.embedding_enabled {
            tx.execute(
                "DELETE FROM workspace_note_embeddings WHERE embedding_model<>?1",
                [&config.embedding_model],
            )?;
        }
        tx.execute(
            "UPDATE embedding_sync_state SET dirty=1,scan_cursor='',revision=revision+1",
            [],
        )?;
        tx.commit()?;
        self.signal_cancelled_jobs(&ids);
        self.wake_scheduler();
        Ok(())
    }
    pub(super) fn claim_background_id(
        &self,
        id: &str,
    ) -> Result<Option<BackgroundClaim>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = tx.query_row("SELECT j.id,j.pulsation_id,j.kind,j.status,j.attempts,j.last_error,j.created_at,j.updated_at,j.started_at,j.finished_at,j.cancel_requested_at,j.available_at FROM jobs j WHERE j.id=?1 AND j.job_class='background' AND j.status='pending' AND j.cancellation_reason IS NULL AND (j.available_at IS NULL OR j.available_at<=?2) AND (j.project_id IS NULL OR EXISTS(SELECT 1 FROM projects p WHERE p.id=j.project_id AND p.is_archived=0))",params![id,self.clock.now().to_rfc3339()],Self::map_job).optional()?;
        let Some(mut job) = job else {
            return Ok(None);
        };
        job.attempts += 1;
        let project: Option<String> =
            tx.query_row("SELECT project_id FROM jobs WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        if !Self::reserve_slot(
            &tx,
            JobClass::Background,
            &Self::job_slot_token(&job),
            project.as_deref(),
            Some(id),
        )? {
            return Ok(None);
        }
        tx.execute("UPDATE jobs SET status='running',generation=generation+1,attempts=attempts+1,started_at=?2,updated_at=?2 WHERE id=?1",params![id,Self::now_iso()])?;
        let generation = tx.query_row("SELECT generation FROM jobs WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
        let cancellation = CancellationToken::new();
        self.register_job_cancellation(id, cancellation.clone());
        if let Err(error) = tx.commit() {
            self.unregister_job_cancellation(id);
            return Err(error.into());
        }
        job.status = JobStatus::Running;
        Ok(Some(BackgroundClaim {
            job,
            generation,
            cancellation,
        }))
    }

    pub(super) fn mark_index_dirty(
        db: &Connection,
        root: &Path,
    ) -> Result<(), PersistenceServiceError> {
        crate::orchestration::publication::safe_target(root, "knowledge/obsidian", false)?;
        let root = root.canonicalize()?.to_string_lossy().to_string();
        db.execute("INSERT INTO embedding_sync_state(workspace_path,next_reconcile_at) VALUES(?1,?2) ON CONFLICT(workspace_path) DO UPDATE SET dirty=1,scan_cursor=''",params![root,Self::now_iso()])?;
        Ok(())
    }

    pub(super) fn enqueue_embedding_path_change(
        &self,
        root: &Path,
        relative_path: &str,
    ) -> Result<(), PersistenceServiceError> {
        let root = root.canonicalize()?;
        let key = root.to_string_lossy().to_string();
        let content = read_optional_document(&root, relative_path)?;
        let desired_hash = content
            .as_ref()
            .filter(|content| workspace_service::is_embedding_document(relative_path, content))
            .map(|content| ContentSha256::of(content.as_bytes()).as_str().to_string());
        let config = self.load_runtime_config()?;
        let mut db = self.connection()?;
        self.qualify_embedding_policy(&db, &key)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let project = tx
            .prepare("SELECT id,workspace_path FROM projects WHERE is_archived=0")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(Result::ok)
            .find(|(_, path)| Path::new(path).canonicalize().ok().as_ref() == Some(&root))
            .map(|(id, _)| id);
        if !config.embedding_enabled {
            if desired_hash.is_none() {
                tx.execute(
                    "DELETE FROM workspace_note_embeddings WHERE workspace_path=?1 AND relative_path=?2",
                    params![key, relative_path],
                )?;
            }
            tx.commit()?;
            return Ok(());
        }
        let prior: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT delta.job_id,delta.desired_hash,job.status FROM embedding_deltas delta JOIN jobs job ON job.id=delta.job_id WHERE delta.workspace_path=?1 AND delta.relative_path=?2 AND delta.embedding_model=?3",
                params![key, relative_path, config.embedding_model],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT revision_hash FROM workspace_note_embeddings WHERE workspace_path=?1 AND relative_path=?2 AND embedding_model=?3 AND sanitization_policy_version=?4",
                params![key, relative_path, config.embedding_model, crate::secret_redactor::SANITIZATION_POLICY_VERSION],
                |row| row.get(0),
            )
            .optional()?;
        if stored == desired_hash
            && prior.as_ref().is_none_or(|(_, hash, status)| {
                hash == &desired_hash && matches!(status.as_str(), "pending" | "running" | "done")
            })
        {
            tx.commit()?;
            return Ok(());
        }
        let mut cancelled = None;
        if let Some((id, prior_hash, status)) = prior {
            if prior_hash == desired_hash && matches!(status.as_str(), "pending" | "running") {
                tx.commit()?;
                return Ok(());
            }
            tx.execute(
                "UPDATE jobs SET status='failed',cancellation_reason='superseded',updated_at=?2 WHERE id=?1 AND status IN ('pending','running','interrupted')",
                params![id, Self::now_iso()],
            )?;
            cancelled = Some(id);
        }
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,job_class,idempotency_key,project_id) VALUES(?1,'index_embedding','pending',0,?2,?2,'background',?1,?3)",
            params![id, Self::now_iso(), project],
        )?;
        tx.execute(
            "INSERT INTO embedding_deltas(workspace_path,relative_path,embedding_model,desired_hash,job_id) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(workspace_path,relative_path,embedding_model) DO UPDATE SET desired_hash=excluded.desired_hash,job_id=excluded.job_id",
            params![key, relative_path, config.embedding_model, desired_hash, id],
        )?;
        tx.commit()?;
        if let Some(cancelled) = cancelled {
            self.signal_cancelled_jobs(&[cancelled]);
        }
        self.wake_scheduler();
        Ok(())
    }

    pub async fn reindex_workspace_embeddings_for_workspace(
        &self,
        workspace: &str,
        request: ReindexWorkspaceEmbeddingsRequest,
    ) -> Result<ReindexWorkspaceEmbeddingsResponse, PersistenceServiceError> {
        let config = self.load_runtime_config()?;
        if !config.embedding_enabled {
            return Err(invalid(
                "Embeddings are off. Turn them on before requesting indexing.",
            ));
        }
        if request
            .model
            .as_deref()
            .is_some_and(|model| model.trim() != config.embedding_model)
        {
            return Err(invalid(
                "Choose and save the embeddings model before reindexing.",
            ));
        }
        let root = PathBuf::from(workspace);
        let service = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut db = service.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Self::mark_index_dirty(&tx, &root)?;
            let key = root.canonicalize()?.to_string_lossy().to_string();
            tx.execute(
                "UPDATE embedding_sync_state SET retry_failed=1 WHERE workspace_path=?1",
                [&key],
            )?;
            tx.commit()?;
            // The UI records intent; only the AppCore supervisor scans and calls providers.
            Ok::<_, PersistenceServiceError>(ReindexWorkspaceEmbeddingsResponse {
                workspace_path: key,
                model: config.embedding_model.clone(),
                queued_count: 0,
                indexed_count: 0,
                skipped_count: 0,
                deleted_count: 0,
                failed_count: 0,
            })
        })
        .await
        .map_err(|e| invalid(e.to_string()))??;
        self.wake_scheduler();
        Ok(result)
    }

    pub(super) fn reconcile_index_batch(
        &self,
        root: &Path,
        model: &str,
        retry_failed: bool,
        stop: &CancellationToken,
    ) -> Result<ReindexWorkspaceEmbeddingsResponse, PersistenceServiceError> {
        crate::orchestration::publication::safe_target(root, "knowledge/obsidian", false)?;
        let root = root.canonicalize()?;
        let key = root.to_string_lossy().to_string();
        let db = self.connection()?;
        self.qualify_embedding_policy(&db, &key)?;
        let (cursor, revision): (String,u64) = db.query_row("SELECT CASE WHEN embedding_model=?2 THEN scan_cursor ELSE '' END, revision FROM embedding_sync_state WHERE workspace_path=?1",params![key,model],|r| Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or_default();
        let retry_failed = retry_failed
            || db
                .query_row(
                    "SELECT retry_failed FROM embedding_sync_state WHERE workspace_path=?1",
                    [&key],
                    |r| r.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
        let paths = crate::repository_context::RepositoryContext::new(&root)
            .map_err(|e| invalid(e.to_string()))?
            .document_paths(true)
            .map_err(|e| invalid(e.to_string()))?
            .into_iter()
            .filter(|p| p.starts_with("knowledge/obsidian/"))
            .collect::<Vec<_>>();
        let mut response = ReindexWorkspaceEmbeddingsResponse {
            workspace_path: key.clone(),
            model: model.into(),
            indexed_count: 0,
            skipped_count: 0,
            deleted_count: 0,
            failed_count: 0,
            queued_count: 0,
        };
        let started = std::time::Instant::now();
        let mut bytes = 0;
        let mut batch = Vec::new();
        let mut last = cursor.clone();
        let mut more = false;
        for path in paths.iter().filter(|p| *p > &cursor) {
            if stop.is_cancelled() {
                return Err(PersistenceServiceError::Cancelled(
                    "Indexing interrupted.".into(),
                ));
            }
            if batch.len() >= 128
                || bytes >= 8 * 1024 * 1024
                || started.elapsed() >= Duration::from_secs(2)
            {
                more = true;
                break;
            }
            last = path.clone();
            match workspace_service::read_context_document(&root, path) {
                Ok(content) => {
                    bytes += content.len();
                    let hash = workspace_service::is_embedding_document(path, &content)
                        .then(|| ContentSha256::of(content.as_bytes()).as_str().to_string());
                    batch.push((path.clone(), hash));
                }
                Err(_) => response.failed_count += 1,
            }
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !Self::embedding_config_matches(&tx, model)? {
            return Ok(response);
        }
        let current_revision: u64 = tx
            .query_row(
                "SELECT revision FROM embedding_sync_state WHERE workspace_path=?1",
                [&key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default();
        if current_revision != revision {
            return Ok(response);
        }
        let project = tx
            .prepare("SELECT id,workspace_path FROM projects WHERE is_archived=0")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(Result::ok)
            .find(|(_, path)| Path::new(path).canonicalize().ok().as_ref() == Some(&root))
            .map(|(id, _)| id);
        // Delete only against a fully enumerated catalog, not against the read batch.
        if !more {
            let known = tx.prepare("SELECT relative_path FROM workspace_note_embeddings WHERE workspace_path=?1 AND embedding_model=?2 UNION SELECT relative_path FROM embedding_deltas WHERE workspace_path=?1 AND embedding_model=?2")?.query_map(params![key,model],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            for path in known {
                if paths.binary_search(&path).is_err() {
                    batch.push((path, None));
                }
            }
        }
        let mut cancelled = Vec::new();
        for (path, hash) in batch {
            let prior: Option<(String,Option<String>,String,Option<String>)> = tx.query_row("SELECT d.job_id,d.desired_hash,j.status,j.cancellation_reason FROM embedding_deltas d JOIN jobs j ON j.id=d.job_id WHERE d.workspace_path=?1 AND d.relative_path=?2 AND d.embedding_model=?3",params![key,path,model],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if prior.as_ref().is_some_and(|(_, h, status, reason)| {
                h == &hash
                    && (matches!(status.as_str(), "pending" | "running" | "interrupted")
                        || (status == "failed"
                            && reason.as_deref() != Some("superseded")
                            && !retry_failed))
            }) {
                response.skipped_count += 1;
                continue;
            }
            let stored: Option<String> = tx.query_row("SELECT revision_hash FROM workspace_note_embeddings WHERE workspace_path=?1 AND relative_path=?2 AND embedding_model=?3 AND sanitization_policy_version=?4",params![key,path,model,crate::secret_redactor::SANITIZATION_POLICY_VERSION],|r|r.get(0)).optional()?;
            if stored == hash && prior.as_ref().is_none_or(|(_, h, _, _)| h == &hash) {
                response.skipped_count += 1;
                continue;
            }
            if let Some((id, _, _, _)) = prior {
                tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',updated_at=?2 WHERE id=?1 AND status IN ('pending','running','interrupted')",params![id,Self::now_iso()])?;
                cancelled.push(id);
            }
            let id = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,job_class,idempotency_key,project_id) VALUES(?1,'index_embedding','pending',0,?2,?2,'background',?1,?3)",params![id,Self::now_iso(),project])?;
            tx.execute("INSERT INTO embedding_deltas(workspace_path,relative_path,embedding_model,desired_hash,job_id) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(workspace_path,relative_path,embedding_model) DO UPDATE SET desired_hash=excluded.desired_hash,job_id=excluded.job_id",params![key,path,model,hash,id])?;
            response.queued_count += 1;
        }
        tx.execute("INSERT INTO embedding_sync_state(workspace_path,dirty,next_reconcile_at,scan_cursor,embedding_model) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(workspace_path) DO UPDATE SET dirty=excluded.dirty,next_reconcile_at=excluded.next_reconcile_at,scan_cursor=excluded.scan_cursor,embedding_model=excluded.embedding_model,last_error=NULL,revision=revision+1,retry_failed=CASE WHEN excluded.dirty=0 THEN 0 ELSE retry_failed END",params![key,more,(self.clock.now()+chrono::Duration::seconds(30)).to_rfc3339(),if more {last}else{String::new()},model])?;
        tx.commit()?;
        self.signal_cancelled_jobs(&cancelled);
        Ok(response)
    }

    fn embedding_config_matches(
        db: &Connection,
        model: &str,
    ) -> Result<bool, PersistenceServiceError> {
        Ok(
            Self::read_runtime_setting(db, SETTING_RUNTIME_EMBEDDING_ENABLED)?.as_deref()
                == Some("true")
                && Self::read_runtime_setting(db, SETTING_RUNTIME_EMBEDDING_MODEL)?
                    .as_deref()
                    .unwrap_or("nomic-embed-text")
                    == model,
        )
    }

    /// Deltas still pending or running for `workspace_key` (already canonicalized) under
    /// `model`. Used as a countdown for `IndexingEvent`, not a fixed total — the queue can grow
    /// between `reconcile_index_batch` passes.
    fn embedding_remaining(
        db: &Connection,
        workspace_key: &str,
        model: &str,
    ) -> Result<usize, PersistenceServiceError> {
        let remaining: i64 = db.query_row(
            "SELECT COUNT(*) FROM jobs j JOIN embedding_deltas d ON d.job_id=j.id \
             WHERE j.job_class='background' AND j.status IN ('pending','running') \
               AND j.kind='index_embedding' AND d.embedding_model=?1 AND d.workspace_path=?2",
            params![model, workspace_key],
            |r| r.get(0),
        )?;
        Ok(remaining.max(0) as usize)
    }

    pub(super) async fn process_indexing(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<bool, PersistenceServiceError> {
        let config = self.load_runtime_config()?;
        if !config.embedding_enabled {
            return Ok(false);
        }
        let service = self.clone();
        let model = config.embedding_model.clone();
        let stop = shutdown.clone();
        let reconciled = tokio::task::spawn_blocking(move || -> Result<bool,PersistenceServiceError> {
            let db = service.connection()?;
            // Registered workspaces are discovered once. Later edits arrive as exact watcher deltas.
            let roots = db.prepare("SELECT workspace_path FROM projects WHERE is_archived=0")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            for root in &roots {
                if crate::orchestration::publication::safe_target(Path::new(root), "knowledge/obsidian", false).is_err() { continue; }
                if let Ok(root)=Path::new(root).canonicalize() {
                    db.execute("INSERT OR IGNORE INTO embedding_sync_state(workspace_path,next_reconcile_at) VALUES(?1,?2)",params![root.to_string_lossy(),service.clock.now().to_rfc3339()])?;
                    service.qualify_embedding_policy(&db, &root.to_string_lossy())?;
                }
            }
            let main_waiting:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runtime_slots WHERE job_class='main') OR EXISTS(SELECT 1 FROM jobs WHERE job_class='main' AND status='pending' AND cancellation_reason IS NULL) OR EXISTS(SELECT 1 FROM artifact_publications WHERE job_class='main' AND state IN ('pending','applying'))",[],|r|r.get(0))?;
            if main_waiting {
                // Foreground has absolute priority; tell the UI why the backlog isn't moving
                // instead of letting it look stalled.
                for root in &roots {
                    let Ok(key) = Path::new(root).canonicalize() else { continue };
                    let key = key.to_string_lossy().to_string();
                    if let Ok(remaining) = Self::embedding_remaining(&db, &key, &model)
                        && remaining > 0
                    {
                        service.emit_indexing_event(IndexingEvent::Paused { workspace_path: key, remaining });
                    }
                }
                return Ok(false);
            }
            let root: Option<String> = db.query_row("SELECT workspace_path FROM embedding_sync_state WHERE dirty=1 OR embedding_model!=?1 ORDER BY workspace_path LIMIT 1",[&model],|r|r.get(0)).optional()?;
            let Some(root)=root else { return Ok(false); };
            let token = format!("index-scan:{}",Uuid::new_v4());
            if !Self::reserve_slot(&db,JobClass::Background,&token,None,None)? { return Ok(false); }
            let _slot=service.slot_guard(token).quiet();
            match service.reconcile_index_batch(Path::new(&root),&model,false,&stop) {
                Ok(_) => {
                    if let Ok(remaining) = Self::embedding_remaining(&db, &root, &model) {
                        service.emit_indexing_event(if remaining == 0 {
                            IndexingEvent::Idle { workspace_path: root.clone() }
                        } else {
                            IndexingEvent::Progress { workspace_path: root.clone(), remaining }
                        });
                    }
                }
                Err(error) => {
                    db.execute("UPDATE embedding_sync_state SET dirty=0,embedding_model=?4,next_reconcile_at=?2,last_error=?3 WHERE workspace_path=?1",params![root,(service.clock.now()+chrono::Duration::seconds(30)).to_rfc3339(),error.to_string(),model])?;
                    service.emit_indexing_event(IndexingEvent::Failed { workspace_path: root.clone(), error: error.to_string() });
                }
            }
            Ok(true)
        }).await.map_err(|e|invalid(e.to_string()))??;
        if shutdown.is_cancelled() {
            return Ok(reconciled);
        }
        let service = self.clone();
        let model = config.embedding_model.clone();
        let claim = tokio::task::spawn_blocking(move || {
            let id:Option<String>=service.connection()?.query_row("SELECT j.id FROM jobs j JOIN embedding_deltas d ON d.job_id=j.id WHERE j.status='pending' AND j.kind='index_embedding' AND d.embedding_model=?1 AND NOT EXISTS(SELECT 1 FROM runtime_slots WHERE job_class='main') AND NOT EXISTS(SELECT 1 FROM jobs foreground WHERE foreground.job_class='main' AND foreground.status='pending' AND foreground.cancellation_reason IS NULL) AND NOT EXISTS(SELECT 1 FROM artifact_publications WHERE job_class='main' AND state IN ('pending','applying')) ORDER BY j.created_at,j.id LIMIT 1",[model],|r|r.get(0)).optional()?;
            id.map(|id|service.claim_background_id(&id)).transpose().map(Option::flatten)
        }).await.map_err(|e|invalid(e.to_string()))??;
        let Some(claim) = claim else {
            return Ok(reconciled);
        };
        let _slot = self.slot_guard(Self::job_slot_token(&claim.job));
        let result = self
            .execute_embedding_delta(&claim, &config, shutdown)
            .await;
        self.unregister_job_cancellation(&claim.job.id);
        if let Err(error) = result {
            self.connection()?.execute("UPDATE jobs SET status=?4,last_error=?2,updated_at=?3 WHERE id=?1 AND generation=?5 AND status='running'",params![claim.job.id,error.to_string(),Self::now_iso(),if shutdown.is_cancelled(){"pending"}else{"failed"},claim.generation])?;
        }
        Ok(true)
    }

    async fn execute_embedding_delta(
        &self,
        claim: &BackgroundClaim,
        config: &AgentRuntimeConfig,
        shutdown: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let workspace_path: Option<String> = self
            .connection()?
            .query_row(
                "SELECT workspace_path FROM embedding_deltas WHERE job_id=?1",
                [&claim.job.id],
                |r| r.get(0),
            )
            .optional()?;
        let result = self
            .execute_embedding_delta_attempt(claim, config, shutdown)
            .await;
        // `Cancelled` covers shutdown and supersession by a newer edit — both benign, not a
        // real failure the developer needs to see.
        if let (Err(error), Some(workspace_path)) = (&result, workspace_path)
            && !matches!(error, PersistenceServiceError::Cancelled(_))
        {
            self.emit_indexing_event(IndexingEvent::Failed {
                workspace_path,
                error: error.to_string(),
            });
        }
        result
    }

    async fn execute_embedding_delta_attempt(
        &self,
        claim: &BackgroundClaim,
        config: &AgentRuntimeConfig,
        shutdown: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        let (root,path,hash):(String,String,Option<String>)=self.connection()?.query_row("SELECT workspace_path,relative_path,desired_hash FROM embedding_deltas WHERE job_id=?1",[&claim.job.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let read_root = root.clone();
        let read_path = path.clone();
        let content = tokio::task::spawn_blocking(move || {
            read_optional_document(Path::new(&read_root), &read_path)
        })
        .await
        .map_err(|e| invalid(e.to_string()))??;
        let current = content
            .as_ref()
            .filter(|c| workspace_service::is_embedding_document(&path, c))
            .map(|c| ContentSha256::of(c.as_bytes()).as_str().to_string());
        let vector = if hash.is_some() && current == hash {
            let sanitization_started = std::time::Instant::now();
            let sanitized =
                crate::secret_redactor::redact_context(content.as_deref().unwrap_or(""))
                    .map_err(invalid)?;
            let project: Option<String> = self.connection()?.query_row(
                "SELECT project_id FROM jobs WHERE id=?1",
                [&claim.job.id],
                |row| row.get(0),
            )?;
            if let Some(project) = project {
                let result = sanitized.result();
                crate::sanitization::SanitizationStore::new(self.database_path())
                    .record_input(
                        &project,
                        None,
                        &format!("embedding:{}:{}", claim.job.id, claim.generation),
                        crate::secret_redactor::SANITIZATION_POLICY_VERSION,
                        result.count,
                        &result.categories,
                        content.as_deref().unwrap_or("").len(),
                        sanitization_started
                            .elapsed()
                            .as_millis()
                            .min(u64::MAX as u128) as u64,
                    )
                    .map_err(invalid)?;
            }
            let prompt = strip_frontmatter_for_embedding(&sanitized.text);
            let vector = tokio::select! {
                _=shutdown.cancelled()=>return Err(PersistenceServiceError::Cancelled("Indexing shut down.".into())),
                _=claim.cancellation.cancelled()=>return Err(PersistenceServiceError::Cancelled("Delta superado.".into())),
                result=tokio::time::timeout(Duration::from_millis(config.timeout_ms), self.embedding_gateway.embed(&config.embedding_model,&prompt))=>result.map_err(|_|invalid("Timeout de embedding."))?.map_err(|e|invalid(e.to_string()))?
            };
            if vector.is_empty() || vector.len() > 8192 || vector.iter().any(|v| !v.is_finite()) {
                return Err(invalid("Invalid vector."));
            }
            Some(serde_json::to_string(&vector).map_err(|e| invalid(e.to_string()))?)
        } else {
            None
        };
        let service = self.clone();
        let id = claim.job.id.clone();
        let generation = claim.generation;
        let model = config.embedding_model.clone();
        tokio::task::spawn_blocking(move|| {
            let mut db=service.connection()?; let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs j JOIN embedding_deltas d ON d.job_id=j.id WHERE j.id=?1 AND j.generation=?2 AND j.status='running' AND j.cancellation_reason IS NULL)",params![id,generation],|r|r.get(0))?;
            if !valid { return Ok(()); }
            let content=read_optional_document(Path::new(&root),&path)?;
            let fresh=content.as_ref().filter(|c|workspace_service::is_embedding_document(&path,c)).map(|c|ContentSha256::of(c.as_bytes()).as_str().to_string());
            let mut processed = false;
            if !Self::embedding_config_matches(&tx,&model)? || fresh!=hash || (hash.is_some() && vector.is_none()) {
                tx.execute("UPDATE jobs SET status='failed',cancellation_reason='superseded',updated_at=?2 WHERE id=?1",params![id,Self::now_iso()])?;
                Self::mark_index_dirty(&tx,Path::new(&root))?;
            } else {
                if let Some(vector)=vector {
                    tx.execute("INSERT INTO workspace_note_embeddings(id,workspace_path,relative_path,revision_hash,embedding_model,embedding_vector_json,updated_at,sanitization_policy_version) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(workspace_path,relative_path,embedding_model) DO UPDATE SET revision_hash=excluded.revision_hash,embedding_vector_json=excluded.embedding_vector_json,updated_at=excluded.updated_at,sanitization_policy_version=excluded.sanitization_policy_version",params![Uuid::new_v4().to_string(),root,path,hash,model,vector,Self::now_iso(),crate::secret_redactor::SANITIZATION_POLICY_VERSION])?;
                } else { tx.execute("DELETE FROM workspace_note_embeddings WHERE workspace_path=?1 AND relative_path=?2 AND embedding_model=?3",params![root,path,model])?; }
                tx.execute("UPDATE jobs SET status='done',finished_at=?2,updated_at=?2 WHERE id=?1",params![id,Self::now_iso()])?;
                processed = true;
            }
            tx.commit()?;
            if processed
                && let Ok(remaining) = Self::embedding_remaining(&db, &root, &model)
            {
                service.emit_indexing_event(if remaining == 0 {
                    IndexingEvent::Idle { workspace_path: root.clone() }
                } else {
                    IndexingEvent::Progress { workspace_path: root.clone(), remaining }
                });
            }
            Ok(())
        }).await.map_err(|e|invalid(e.to_string()))?
    }
}
fn read_optional_document(
    root: &Path,
    relative: &str,
) -> Result<Option<String>, PersistenceServiceError> {
    let path = crate::orchestration::publication::safe_target(root, relative, false)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => workspace_service::read_context_document(root, relative)
            .map(Some)
            .map_err(|e| invalid(format!("{e:?}"))),
    }
}

#[cfg(test)]
#[path = "indexing_tests.rs"]
mod tests;
