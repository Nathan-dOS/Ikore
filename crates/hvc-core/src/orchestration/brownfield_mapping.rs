//! Persistent BF2 structural cache and low-priority maintenance job.

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use rusqlite::TransactionBehavior;

use super::*;
use crate::orchestration::{
    CollectionBudget, ReferenceRelation, ReferenceResolution, SourceLanguage, SourceSnapshot,
    SourceStructuralFacts, StructuralParseStatus, TREE_SITTER_PARSER_VERSION, TreeSitterExtractor,
};
use crate::repository_context::RepositoryContext;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldMappingMetrics {
    pub cache_hits: u32,
    pub cache_misses: u32,
    pub mapped_sources: u32,
    pub partial_sources: u32,
    pub unsupported_sources: u32,
    pub removed_sources: u32,
    pub coverage_partial: bool,
    pub diagnostics: Vec<String>,
    pub elapsed_us: u64,
}

impl PersistenceService {
    pub fn enqueue_brownfield_mapping(
        &self,
        project_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::find_project_by_id(&tx, &project_id, false)?.ok_or_else(|| {
            PersistenceServiceError::NotFound(
                "Projeto ativo não encontrado para o mapeamento brownfield.".into(),
            )
        })?;
        if Self::read_runtime_setting(&tx, &brownfield_consent_key(&project_id))?.as_deref()
            != Some("true")
        {
            return Err(PersistenceServiceError::Validation(
                "O mapeamento brownfield exige consentimento ativo para este projeto.".into(),
            ));
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM jobs WHERE project_id=?1 AND kind='brownfield_map' AND status IN ('pending','running','interrupted') AND cancellation_reason IS NULL ORDER BY created_at,id LIMIT 1",
                [&project_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            tx.commit()?;
            return Ok(existing);
        }
        let id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,project_id,job_class,idempotency_key) VALUES(?1,'brownfield_map','pending',0,?2,?2,?3,'background',?4)",
            params![id, now, project_id, format!("brownfield-map:{project_id}:{id}")],
        )?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(id)
    }

    pub(super) fn cancel_brownfield_mapping(
        &self,
        project_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = tx
            .prepare("SELECT id FROM jobs WHERE project_id=?1 AND kind IN ('brownfield_map','brownfield_enrich') AND status IN ('pending','running','interrupted')")?
            .query_map([project_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let now = Self::now_iso();
        tx.execute(
            "UPDATE jobs SET status='failed',cancellation_reason='superseded',finished_at=?2,updated_at=?2 WHERE project_id=?1 AND kind IN ('brownfield_map','brownfield_enrich') AND status IN ('pending','running','interrupted')",
            params![project_id, Self::now_iso()],
        )?;
        tx.execute(
            "UPDATE brownfield_enrichment_runs SET state='cancelled',diagnostic='Consentimento brownfield revogado.',updated_at=?2 WHERE project_id=?1 AND state IN ('pending','running')",
            params![project_id,now],
        )?;
        tx.execute(
            "UPDATE artifact_publications SET state='cancelled',last_error='Consentimento brownfield revogado.',updated_at=?2 WHERE project_id=?1 AND artifact_id IN (SELECT publication_artifact_id FROM brownfield_component_notes WHERE project_id=?1) AND state IN ('pending','applying')",
            params![project_id,now],
        )?;
        tx.execute(
            "UPDATE artifacts SET publication_state='not_required' WHERE id IN (SELECT publication_artifact_id FROM brownfield_component_notes WHERE project_id=?1 AND publication_state IN ('pending','publishing'))",
            [project_id],
        )?;
        tx.execute(
            "UPDATE brownfield_component_notes SET publication_state='not_scheduled',diagnostic='Consentimento brownfield revogado.',updated_at=?2 WHERE project_id=?1 AND publication_state IN ('pending','publishing')",
            params![project_id,now],
        )?;
        tx.commit()?;
        self.signal_cancelled_jobs(&ids);
        self.wake_scheduler();
        Ok(())
    }

    pub(super) async fn process_brownfield_mapping(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<bool, PersistenceServiceError> {
        if shutdown.is_cancelled() {
            return Ok(false);
        }
        let service = self.clone();
        let claim = tokio::task::spawn_blocking(move || {
            let id: Option<String> = service
                .connection()?
                .query_row(
                    "SELECT j.id FROM jobs j JOIN runtime_settings s ON s.key='brownfield.onboarding_consent.'||j.project_id AND s.value='true' WHERE j.kind='brownfield_map' AND j.job_class='background' AND j.status='pending' AND j.cancellation_reason IS NULL ORDER BY j.created_at,j.id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            id.map(|id| service.claim_background_id(&id))
                .transpose()
                .map(Option::flatten)
        })
        .await
        .map_err(|error| coordination::invalid(error.to_string()))??;
        let Some(claim) = claim else {
            return Ok(false);
        };
        let _slot = self.slot_guard(Self::job_slot_token(&claim.job));
        let service = self.clone();
        let work = claim.job.clone();
        let generation = claim.generation;
        let cancellation = claim.cancellation.clone();
        let worker_cancellation = cancellation.clone();
        let task = tokio::task::spawn_blocking(move || {
            service.execute_brownfield_mapping(&work, generation, &worker_cancellation)
        });
        tokio::pin!(task);
        let result = tokio::select! {
            result = &mut task => result.map_err(|error| coordination::invalid(error.to_string()))?,
            _ = shutdown.cancelled() => {
                cancellation.cancel();
                task.await.map_err(|error| coordination::invalid(error.to_string()))?
            }
        };
        self.unregister_job_cancellation(&claim.job.id);
        if let Err(error) = &result {
            let shutdown_interruption = shutdown.is_cancelled();
            self.connection()?.execute(
                "UPDATE jobs SET status=?2,last_error=?3,started_at=CASE WHEN ?4 THEN NULL ELSE started_at END,finished_at=CASE WHEN ?4 THEN NULL ELSE ?5 END,cancellation_reason=CASE WHEN ?4 THEN NULL ELSE cancellation_reason END,cancel_requested_at=CASE WHEN ?4 THEN NULL ELSE cancel_requested_at END,available_at=CASE WHEN ?4 THEN ?5 ELSE available_at END,updated_at=?5 WHERE id=?1 AND generation=?6 AND status='running'",
                params![
                    claim.job.id,
                    if shutdown_interruption { "pending" } else { "failed" },
                    error.to_string(),
                    shutdown_interruption,
                    Self::now_iso(),
                    claim.generation
                ],
            )?;
        }
        result.map(|_| true)
    }

    fn execute_brownfield_mapping(
        &self,
        job: &Job,
        generation: u64,
        cancellation: &CancellationToken,
    ) -> Result<BrownfieldMappingMetrics, PersistenceServiceError> {
        let db = self.connection()?;
        let (project_id, root): (String, String) = db.query_row(
            "SELECT j.project_id,p.workspace_path FROM jobs j JOIN projects p ON p.id=j.project_id JOIN runtime_settings s ON s.key='brownfield.onboarding_consent.'||j.project_id AND s.value='true' WHERE j.id=?1 AND j.kind='brownfield_map' AND j.status='running' AND j.generation=?2 AND j.cancellation_reason IS NULL AND p.is_archived=0",
            params![job.id, generation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let repository = RepositoryContext::new(Path::new(&root))
            .map_err(|error| coordination::invalid(error.to_string()))?;
        let budget = CollectionBudget::default();
        let started = Instant::now();
        let (sources, coverage) = repository
            .capture_code_sources(&project_id, budget, cancellation)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        let extractor = TreeSitterExtractor;
        let mut metrics = BrownfieldMappingMetrics {
            coverage_partial: coverage.state != crate::orchestration::CoverageState::Complete,
            diagnostics: coverage.diagnostics.clone(),
            ..BrownfieldMappingMetrics::default()
        };
        let mut observed_paths = HashSet::new();
        let mut processed_all_sources = true;

        for source in &sources {
            if cancellation.is_cancelled() {
                return Err(PersistenceServiceError::Cancelled(
                    "Mapeamento brownfield cancelado.".into(),
                ));
            }
            if started.elapsed().as_millis() >= u128::from(budget.max_duration_ms) {
                processed_all_sources = false;
                metrics.coverage_partial = true;
                metrics.diagnostics.push(
                    "budget_exhausted: manutenção estrutural preservou o cache não visitado."
                        .into(),
                );
                break;
            }
            observed_paths.insert(source.revision().relative_path.as_str().to_string());
            if Self::cached_source_matches(
                &db,
                source,
                TREE_SITTER_PARSER_VERSION,
                SourceLanguage::detect(&source.revision().relative_path).query_version(),
            )? {
                metrics.cache_hits += 1;
                continue;
            }
            metrics.cache_misses += 1;
            let source_facts = extractor
                .extract_source(source, cancellation)
                .map_err(|error| coordination::invalid(error.to_string()))?;
            match source_facts.status {
                StructuralParseStatus::Mapped => metrics.mapped_sources += 1,
                StructuralParseStatus::Partial => metrics.partial_sources += 1,
                StructuralParseStatus::Unsupported => metrics.unsupported_sources += 1,
            }
            self.replace_source_structure(job, generation, &source_facts, cancellation)?;
        }

        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::validate_mapping_fence(&tx, &job.id, generation, &project_id)?;
        if coverage.state == crate::orchestration::CoverageState::Complete
            && processed_all_sources
            && !cancellation.is_cancelled()
        {
            let cached = tx
                .prepare("SELECT relative_path FROM brownfield_source_cache WHERE project_id=?1")?
                .query_map([&project_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for path in cached {
                if !observed_paths.contains(&path) {
                    Self::mark_component_notes_stale(&tx, &project_id, &path, None)?;
                    metrics.removed_sources += tx.execute(
                        "DELETE FROM brownfield_source_cache WHERE project_id=?1 AND relative_path=?2",
                        params![project_id, path],
                    )? as u32;
                }
            }
        }
        metrics.elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let output = serde_json::to_string(&metrics)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        tx.execute(
            "UPDATE jobs SET status='done',maintenance_metrics_json=?2,finished_at=?3,updated_at=?3 WHERE id=?1 AND generation=?4 AND status='running'",
            params![job.id, output, Self::now_iso(), generation],
        )?;
        tx.commit()?;
        Ok(metrics)
    }

    fn cached_source_matches(
        db: &Connection,
        source: &SourceSnapshot,
        parser_version: &str,
        query_version: &str,
    ) -> Result<bool, PersistenceServiceError> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM brownfield_source_cache WHERE project_id=?1 AND relative_path=?2 AND content_sha256=?3 AND parser_version=?4 AND query_version=?5)",
            params![
                source.revision().project_id,
                source.revision().relative_path.as_str(),
                source.revision().content_sha256.as_str(),
                parser_version,
                query_version,
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    fn replace_source_structure(
        &self,
        job: &Job,
        generation: u64,
        facts: &SourceStructuralFacts,
        cancellation: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        if cancellation.is_cancelled() {
            return Err(PersistenceServiceError::Cancelled(
                "Mapeamento brownfield cancelado.".into(),
            ));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::validate_mapping_fence(&tx, &job.id, generation, &facts.source.project_id)?;
        let symbols = serde_json::to_string(&facts.symbols)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        let references = serde_json::to_string(&facts.references)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        let now = Self::now_iso();

        Self::mark_component_notes_stale(
            &tx,
            &facts.source.project_id,
            facts.source.relative_path.as_str(),
            Some(facts.source.content_sha256.as_str()),
        )?;

        tx.execute(
            "DELETE FROM brownfield_edges WHERE project_id=?1 AND source_path=?2",
            params![facts.source.project_id, facts.source.relative_path.as_str()],
        )?;
        tx.execute(
            "INSERT INTO brownfield_source_cache(project_id,relative_path,content_sha256,language,parser_version,query_version,status,symbols_json,references_json,parse_elapsed_us,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(project_id,relative_path) DO UPDATE SET content_sha256=excluded.content_sha256,language=excluded.language,parser_version=excluded.parser_version,query_version=excluded.query_version,status=excluded.status,symbols_json=excluded.symbols_json,references_json=excluded.references_json,parse_elapsed_us=excluded.parse_elapsed_us,updated_at=excluded.updated_at",
            params![
                facts.source.project_id,
                facts.source.relative_path.as_str(),
                facts.source.content_sha256.as_str(),
                facts.language.as_str(),
                facts.parser_version,
                facts.query_version,
                facts.status.as_str(),
                symbols,
                references,
                facts.parse_elapsed_us,
                now,
            ],
        )?;
        for reference in &facts.references {
            let (resolution, destination): (&str, Option<&str>) = match &reference.resolution {
                ReferenceResolution::Unresolved { .. } => ("unresolved", None),
                ReferenceResolution::External { .. } => ("external", None),
                ReferenceResolution::Resolved { target } => ("resolved", Some(target.as_str())),
            };
            let relation = match reference.relation {
                ReferenceRelation::Import => "import",
                ReferenceRelation::Call => "call",
                ReferenceRelation::Lexical => "lexical",
            };
            tx.execute(
                "INSERT INTO brownfield_edges(project_id,source_path,relation,observed_text,line,destination_path,resolution,source_revision_sha256,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    facts.source.project_id,
                    facts.source.relative_path.as_str(),
                    relation,
                    reference.observed_text,
                    reference.line,
                    destination,
                    resolution,
                    facts.source.content_sha256.as_str(),
                    now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn mark_component_notes_stale(
        db: &Connection,
        project_id: &str,
        relative_path: &str,
        current_revision: Option<&str>,
    ) -> Result<(), PersistenceServiceError> {
        let affected = db
            .prepare(
                "SELECT note.id,note.publication_artifact_id,note.publication_state FROM brownfield_component_sources source JOIN brownfield_component_notes note ON note.id=source.note_id WHERE source.project_id=?1 AND source.relative_path=?2 AND note.mapping_status<>'stale' AND (?3 IS NULL OR source.observed_revision_sha256<>?3)",
            )?
            .query_map(params![project_id, relative_path, current_revision], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if affected.is_empty() {
            return Ok(());
        }
        let diagnostic = format!(
            "Fonte `{relative_path}` mudou desde a revisão observada; regeneração não foi agendada."
        );
        let now = Self::now_iso();
        for (note_id, artifact_id, publication_state) in affected {
            let not_yet_published = matches!(publication_state.as_str(), "pending" | "publishing");
            db.execute(
                "UPDATE brownfield_component_notes SET mapping_status='stale',publication_state=CASE WHEN ?2 THEN 'not_scheduled' ELSE publication_state END,diagnostic=?3,updated_at=?4 WHERE id=?1",
                params![note_id, not_yet_published, diagnostic, now],
            )?;
            if not_yet_published && let Some(artifact_id) = artifact_id {
                db.execute(
                    "UPDATE artifact_publications SET state='cancelled',last_error=?2,updated_at=?3 WHERE artifact_id=?1 AND state IN ('pending','applying')",
                    params![artifact_id, diagnostic, now],
                )?;
                db.execute(
                    "UPDATE artifacts SET publication_state='not_required' WHERE id=?1 AND publication_state IN ('pending','applying')",
                    [artifact_id],
                )?;
            }
        }
        Ok(())
    }

    fn validate_mapping_fence(
        db: &Connection,
        job_id: &str,
        generation: u64,
        project_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let valid: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs j JOIN runtime_settings s ON s.key='brownfield.onboarding_consent.'||j.project_id AND s.value='true' WHERE j.id=?1 AND j.project_id=?2 AND j.kind='brownfield_map' AND j.status='running' AND j.generation=?3 AND j.cancellation_reason IS NULL)",
            params![job_id, project_id, generation],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(PersistenceServiceError::Cancelled(
                "Mapeamento brownfield superado ou sem consentimento.".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence_service::CreateProjectRequest;
    use std::fs;

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PersistenceService,
        String,
    ) {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(
            workspace.path().join("src/lib.rs"),
            "use crate::auth::Token;\nfn run() { old_call(); }\n",
        )
        .unwrap();
        let service = PersistenceService::initialize(data.path()).unwrap();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("Brownfield".into()),
            })
            .unwrap()
            .project;
        service
            .set_brownfield_onboarding_consent(&project.id, true)
            .unwrap();
        (data, workspace, service, project.id)
    }

    async fn run_mapping(service: &PersistenceService, project: &str) -> BrownfieldMappingMetrics {
        let id = service.enqueue_brownfield_mapping(project).unwrap();
        assert!(
            service
                .process_brownfield_mapping(&CancellationToken::new())
                .await
                .unwrap()
        );
        let output: String = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT maintenance_metrics_json FROM jobs WHERE id=?1 AND status='done'",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&output).unwrap()
    }

    #[tokio::test]
    async fn cold_then_warm_mapping_reports_misses_then_hits() {
        let (_data, _workspace, service, project) = fixture();
        let cold = run_mapping(&service, &project).await;
        assert_eq!(cold.cache_misses, 1);
        assert_eq!(cold.cache_hits, 0);
        let warm = run_mapping(&service, &project).await;
        assert_eq!(warm.cache_hits, 1);
        assert_eq!(warm.cache_misses, 0);
        assert!(cold.elapsed_us > 0);
        assert!(warm.elapsed_us > 0);

        let interrupted = service.enqueue_brownfield_mapping(&project).unwrap();
        service
            .connection()
            .unwrap()
            .execute(
                "UPDATE jobs SET status='running',cancellation_reason='shutdown' WHERE id=?1",
                [&interrupted],
            )
            .unwrap();
        service.recover_interrupted_jobs().unwrap();
        let (status, reason): (String, Option<String>) = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT status,cancellation_reason FROM jobs WHERE id=?1",
                [&interrupted],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(reason, None);
    }

    #[tokio::test]
    async fn body_hash_change_replaces_edges_instead_of_accumulating() {
        let (_data, workspace, service, project) = fixture();
        run_mapping(&service, &project).await;
        fs::write(
            workspace.path().join("src/lib.rs"),
            "use crate::auth::Token;\nfn run() { new_call(); }\n",
        )
        .unwrap();
        let changed = run_mapping(&service, &project).await;
        assert_eq!(changed.cache_misses, 1);
        let db = service.connection().unwrap();
        let edges = db
            .prepare("SELECT observed_text FROM brownfield_edges WHERE project_id=?1 AND source_path='src/lib.rs' AND relation='call'")
            .unwrap()
            .query_map([&project], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(edges, ["new_call"]);
    }

    #[tokio::test]
    async fn cache_is_project_scoped_and_parser_or_query_changes_are_misses() {
        let (_data, workspace, service, first) = fixture();
        run_mapping(&service, &first).await;
        let second = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace
                    .path()
                    .join("second")
                    .to_string_lossy()
                    .into_owned(),
                name: Some("Second".into()),
            })
            .unwrap()
            .project;
        fs::create_dir_all(workspace.path().join("second/src")).unwrap();
        fs::write(
            workspace.path().join("second/src/lib.rs"),
            "fn second() {}\n",
        )
        .unwrap();
        service
            .set_brownfield_onboarding_consent(&second.id, true)
            .unwrap();
        run_mapping(&service, &second.id).await;
        let db = service.connection().unwrap();
        let count: u32 = db
            .query_row(
                "SELECT COUNT(*) FROM brownfield_source_cache WHERE relative_path='src/lib.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        let snapshot = SourceSnapshot::from_text(
            &first,
            crate::orchestration::RelativeSourcePath::new("src/lib.rs").unwrap(),
            "use crate::auth::Token;\nfn run() { old_call(); }\n".into(),
        )
        .unwrap();
        assert!(
            !PersistenceService::cached_source_matches(
                &db,
                &snapshot,
                "future-parser-version",
                SourceLanguage::Rust.query_version(),
            )
            .unwrap()
        );
        assert!(
            !PersistenceService::cached_source_matches(
                &db,
                &snapshot,
                TREE_SITTER_PARSER_VERSION,
                "future-query-version",
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn revocation_cancels_pending_mapping_without_deleting_cache() {
        let (_data, _workspace, service, project) = fixture();
        run_mapping(&service, &project).await;
        let cached_before: u32 = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM brownfield_source_cache WHERE project_id=?1",
                [&project],
                |row| row.get(0),
            )
            .unwrap();
        let id = service.enqueue_brownfield_mapping(&project).unwrap();
        service
            .set_brownfield_onboarding_consent(&project, false)
            .unwrap();
        let status: String = service
            .connection()
            .unwrap()
            .query_row("SELECT status FROM jobs WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        let cached_after: u32 = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM brownfield_source_cache WHERE project_id=?1",
                [&project],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(cached_after, cached_before);
    }
}
