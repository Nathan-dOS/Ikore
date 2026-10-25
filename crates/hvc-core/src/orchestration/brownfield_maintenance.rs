//! BF4 discovery frontier, exact refresh events and user-controlled ignore state.

use std::path::Path;

use rusqlite::TransactionBehavior;

use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, Uuid,
    brownfield_consent_key, coordination, params,
};
use crate::orchestration::brownfield::{
    BrownfieldCoverageMetrics, BrownfieldFrontierEntry, BrownfieldFrontierStatus,
    BrownfieldOnboardingStatus,
};
use crate::orchestration::{RelativeSourcePath, SourceRevision};
use crate::repository_context::RepositoryContext;
use crate::workspace_service::WorkspaceChangedEvent;

const DELETED_REVISION_KEY: &str = "deleted";

impl PersistenceService {
    pub(super) fn brownfield_coverage_metrics(
        db: &Connection,
        project_id: &str,
    ) -> Result<BrownfieldCoverageMetrics, PersistenceServiceError> {
        let frontier_initialized = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM brownfield_frontier_state WHERE project_id=?1 AND initialized=1)",
            [project_id],
            |row| row.get(0),
        )?;
        let (known, ignored, mapped, partial, stale, failed): (i64, i64, i64, i64, i64, i64) =
            db.query_row(
                "SELECT COUNT(*),COALESCE(SUM(CASE WHEN status='ignored' THEN 1 ELSE 0 END),0),COALESCE(SUM(CASE WHEN status='mapped' THEN 1 ELSE 0 END),0),COALESCE(SUM(CASE WHEN status='partial' THEN 1 ELSE 0 END),0),COALESCE(SUM(CASE WHEN status='stale' THEN 1 ELSE 0 END),0),COALESCE(SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END),0) FROM brownfield_frontier WHERE project_id=?1",
                [project_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
        let (documented_stale, documented_validated, documented_proposed): (i64, i64, i64) =
            db.query_row(
                "WITH source_states AS (SELECT frontier.relative_path,COALESCE(MAX(CASE WHEN note.mapping_status='stale' THEN 1 ELSE 0 END),0) AS has_stale,COALESCE(MAX(CASE WHEN note.mapping_status='validated' THEN 1 ELSE 0 END),0) AS has_validated,COALESCE(MAX(CASE WHEN note.mapping_status='proposed' THEN 1 ELSE 0 END),0) AS has_proposed FROM brownfield_frontier frontier LEFT JOIN brownfield_component_sources source ON source.project_id=frontier.project_id AND source.relative_path=frontier.relative_path LEFT JOIN brownfield_component_notes note ON note.id=source.note_id AND note.project_id=frontier.project_id WHERE frontier.project_id=?1 AND frontier.status<>'ignored' GROUP BY frontier.relative_path) SELECT COALESCE(SUM(has_stale),0),COALESCE(SUM(CASE WHEN has_stale=0 AND has_validated=1 THEN 1 ELSE 0 END),0),COALESCE(SUM(CASE WHEN has_stale=0 AND has_validated=0 AND has_proposed=1 THEN 1 ELSE 0 END),0) FROM source_states",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let resolution_state = db
            .query_row(
                "SELECT state,resolver_version,elapsed_us FROM brownfield_resolution_state WHERE project_id=?1",
                [project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let (resolved, ambiguous, external, unresolved): (i64, i64, i64, i64) = db
            .query_row(
                "SELECT COALESCE(SUM(edge.resolution='resolved'),0),COALESCE(SUM(edge.resolution='ambiguous'),0),COALESCE(SUM(edge.resolution='external'),0),COALESCE(SUM(edge.resolution='unresolved'),0) FROM brownfield_edges edge LEFT JOIN brownfield_frontier frontier ON frontier.project_id=edge.project_id AND frontier.relative_path=edge.source_path WHERE edge.project_id=?1 AND COALESCE(frontier.status,'mapped')<>'ignored'",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let count = |value: i64| value.clamp(0, i64::from(u32::MAX)) as u32;
        let inventory_known_sources = count(known);
        let ignored_sources = count(ignored);
        let eligible_source_denominator = inventory_known_sources.saturating_sub(ignored_sources);
        let extracted_valid_sources = count(mapped).min(eligible_source_denominator);
        let extraction_progress_basis_points = extracted_valid_sources
            .saturating_mul(BrownfieldCoverageMetrics::SCALE_BASIS_POINTS)
            .checked_div(eligible_source_denominator)
            .unwrap_or(0);
        Ok(BrownfieldCoverageMetrics {
            frontier_initialized,
            inventory_known_sources,
            eligible_source_denominator,
            ignored_sources,
            extracted_valid_sources,
            extracted_partial_sources: count(partial),
            extracted_stale_sources: count(stale),
            extraction_failed_sources: count(failed),
            extraction_progress_basis_points,
            documentation_proposed_sources: count(documented_proposed),
            documentation_validated_sources: count(documented_validated),
            documentation_stale_sources: count(documented_stale),
            resolution_complete: resolution_state
                .as_ref()
                .is_some_and(|(state, _, _)| state == "complete"),
            resolution_state: resolution_state.as_ref().map(|(state, _, _)| state.clone()),
            resolver_version: resolution_state
                .as_ref()
                .map(|(_, version, _)| version.clone()),
            resolved_references: count(resolved),
            ambiguous_references: count(ambiguous),
            external_references: count(external),
            unresolved_references: count(unresolved),
            resolution_elapsed_us: resolution_state
                .map_or(0, |(_, _, elapsed)| elapsed.max(0) as u64),
        })
    }

    pub fn list_brownfield_frontier(
        &self,
        project_id: &str,
    ) -> Result<Vec<BrownfieldFrontierEntry>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let db = self.connection()?;
        Self::find_project_by_id(&db, &project_id, false)?.ok_or_else(|| {
            PersistenceServiceError::NotFound("Active brownfield project not found.".into())
        })?;
        db.prepare(
            "SELECT project_id,relative_path,status,content_sha256,in_degree,priority_reason,budget_consumed,attempts,last_used_at FROM brownfield_frontier WHERE project_id=?1 ORDER BY relative_path",
        )?
        .query_map([project_id], |row| {
            let raw = row.get::<_, String>(2)?;
            let status = frontier_status(&raw).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    format!("invalid boundary status: {raw}").into(),
                )
            })?;
            Ok(BrownfieldFrontierEntry {
                project_id: row.get(0)?,
                relative_path: row.get(1)?,
                status,
                content_sha256: row.get(3)?,
                in_degree: row.get(4)?,
                priority_reason: row.get(5)?,
                budget_consumed: row.get(6)?,
                attempts: row.get(7)?,
                last_used_at: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub fn set_brownfield_path_ignored(
        &self,
        project_id: &str,
        relative_path: &str,
        ignored: bool,
    ) -> Result<BrownfieldOnboardingStatus, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let relative_path = RelativeSourcePath::new(relative_path)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        let project = self.active_project_by_id(&project_id)?;
        if Self::read_runtime_setting(&self.connection()?, &brownfield_consent_key(&project_id))?
            .as_deref()
            != Some("true")
        {
            return Err(coordination::invalid(
                "Ignoring modules requires active brownfield consent.",
            ));
        }
        if !RepositoryContext::is_code_relative_path(relative_path.as_str()) {
            return Err(coordination::invalid(
                "Only eligible code sources can be ignored.",
            ));
        }
        let revision = if ignored {
            None
        } else {
            Self::capture_brownfield_revision(
                Path::new(&project.workspace_path),
                &project_id,
                &relative_path,
            )?
        };
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Self::now_iso();
        if ignored {
            tx.execute(
                "INSERT INTO brownfield_frontier(project_id,relative_path,status,priority_reason,priority_since,created_at,updated_at) VALUES(?1,?2,'ignored','ignored_by_user',?3,?3,?3) ON CONFLICT(project_id,relative_path) DO UPDATE SET status='ignored',priority_reason='ignored_by_user',updated_at=excluded.updated_at",
                params![project_id, relative_path.as_str(), now],
            )?;
            tx.execute(
                "UPDATE brownfield_refresh_queue SET state='ignored',diagnostic='Module ignored by the user.',updated_at=?3 WHERE project_id=?1 AND relative_path=?2 AND state IN ('pending','in_progress')",
                params![project_id, relative_path.as_str(), now],
            )?;
        } else {
            tx.execute(
                "INSERT INTO brownfield_frontier(project_id,relative_path,status,content_sha256,priority_reason,priority_since,created_at,updated_at) VALUES(?1,?2,'stale',?3,'reactivated_by_user',?4,?4,?4) ON CONFLICT(project_id,relative_path) DO UPDATE SET status='stale',content_sha256=excluded.content_sha256,priority_reason='reactivated_by_user',priority_since=excluded.priority_since,updated_at=excluded.updated_at",
                params![project_id, relative_path.as_str(), revision.as_ref().map(|value| value.content_sha256.as_str()), now],
            )?;
            Self::enqueue_brownfield_refresh_tx(
                &tx,
                &project_id,
                relative_path.as_str(),
                revision.as_ref().map(|value| value.content_sha256.as_str()),
                "user_reactivated",
            )?;
            Self::ensure_brownfield_mapping_job_tx(&tx, &project_id)?;
        }
        tx.commit()?;
        self.ensure_brownfield_resolution_job(&project_id)?;
        self.wake_scheduler();
        self.get_brownfield_onboarding(&project_id)
    }

    pub(crate) fn record_workspace_change(
        &self,
        workspace_root: &Path,
        event: &WorkspaceChangedEvent,
    ) -> Result<(), PersistenceServiceError> {
        let root = workspace_root.canonicalize()?;
        let db = self.connection()?;
        let project = db
            .prepare("SELECT id,workspace_path FROM projects WHERE is_archived=0")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(Result::ok)
            .find(|(_, path)| Path::new(path).canonicalize().ok().as_ref() == Some(&root));
        let Some((project_id, _)) = project else {
            return Ok(());
        };
        // ADR-034 M5: a hash comparison only; it never blocks the maintenance below.
        if let Err(error) =
            self.record_doc_drift_change(&db, &project_id, &root, &event.relative_path)
        {
            eprintln!("documentation drift detection skipped safely: {error}");
        }
        let consented = Self::read_runtime_setting(&db, &brownfield_consent_key(&project_id))?
            .as_deref()
            == Some("true");

        if RepositoryContext::is_code_relative_path(&event.relative_path) && consented {
            let path = RelativeSourcePath::new(event.relative_path.clone())
                .map_err(|error| coordination::invalid(error.to_string()))?;
            let revision = Self::capture_brownfield_revision(&root, &project_id, &path)?;
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            Self::enqueue_brownfield_refresh_tx(
                &tx,
                &project_id,
                path.as_str(),
                revision.as_ref().map(|value| value.content_sha256.as_str()),
                &event.kind,
            )?;
            Self::ensure_brownfield_mapping_job_tx(&tx, &project_id)?;
            tx.commit()?;
            self.wake_scheduler();
        }

        let normalized_path = event.relative_path.replace('\\', "/").to_ascii_lowercase();
        if consented
            && matches!(
                normalized_path.rsplit('/').next(),
                Some("cargo.toml" | "package.json" | "tsconfig.json" | "jsconfig.json")
            )
        {
            self.ensure_brownfield_resolution_job(&project_id)?;
        }

        if normalized_path.starts_with("knowledge/obsidian/components/") {
            self.synchronize_component_mapping_status(&db, &root, &project_id, event)?;
            self.enqueue_embedding_path_change(&root, &event.relative_path)?;
        }
        Ok(())
    }

    fn synchronize_component_mapping_status(
        &self,
        db: &Connection,
        workspace_root: &Path,
        project_id: &str,
        event: &WorkspaceChangedEvent,
    ) -> Result<(), PersistenceServiceError> {
        let target = crate::orchestration::publication::safe_target(
            workspace_root,
            &event.relative_path,
            false,
        )?;
        let mapping_status = if target.is_file() {
            let content = crate::workspace_service::read_context_document(
                workspace_root,
                &event.relative_path,
            )
            .map_err(|error| coordination::invalid(format!("Invalid component note: {error:?}")))?;
            crate::workspace_service::component_mapping_status(&event.relative_path, &content)
        } else {
            Some("stale")
        };
        if let Some(mapping_status) = mapping_status {
            db.execute(
                "UPDATE brownfield_component_notes SET mapping_status=?3,updated_at=?4 WHERE project_id=?1 AND relative_path=?2",
                params![project_id, event.relative_path.replace('\\', "/"), mapping_status, Self::now_iso()],
            )?;
        }
        Ok(())
    }

    pub(super) fn capture_brownfield_revision(
        root: &Path,
        project_id: &str,
        relative_path: &RelativeSourcePath,
    ) -> Result<Option<SourceRevision>, PersistenceServiceError> {
        let target =
            crate::orchestration::publication::safe_target(root, relative_path.as_str(), false)?;
        match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(metadata) if !metadata.is_file() => Ok(None),
            Ok(_) => RepositoryContext::new(root)
                .and_then(|repository| repository.capture_code_source(project_id, relative_path))
                .map(|snapshot| Some(snapshot.revision().clone()))
                .map_err(|error| coordination::invalid(error.to_string())),
        }
    }

    pub(super) fn enqueue_brownfield_refresh_tx(
        tx: &Connection,
        project_id: &str,
        relative_path: &str,
        content_sha256: Option<&str>,
        event_kind: &str,
    ) -> Result<bool, PersistenceServiceError> {
        let ignored: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM brownfield_frontier WHERE project_id=?1 AND relative_path=?2 AND status='ignored')",
            params![project_id, relative_path],
            |row| row.get(0),
        )?;
        if ignored {
            return Ok(false);
        }
        let cached: Option<String> = tx
            .query_row(
                "SELECT content_sha256 FROM brownfield_source_cache WHERE project_id=?1 AND relative_path=?2",
                params![project_id, relative_path],
                |row| row.get(0),
            )
            .optional()?;
        if cached.as_deref() == content_sha256 {
            return Ok(false);
        }
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO brownfield_frontier(project_id,relative_path,status,content_sha256,priority_reason,priority_since,created_at,updated_at) VALUES(?1,?2,'stale',?3,'workspace_change',?4,?4,?4) ON CONFLICT(project_id,relative_path) DO UPDATE SET status=CASE WHEN status='ignored' THEN status ELSE 'stale' END,content_sha256=CASE WHEN status='ignored' THEN content_sha256 ELSE excluded.content_sha256 END,priority_reason=CASE WHEN status='ignored' THEN priority_reason ELSE 'workspace_change' END,priority_since=CASE WHEN status='ignored' THEN priority_since ELSE excluded.priority_since END,updated_at=excluded.updated_at",
            params![project_id, relative_path, content_sha256, now],
        )?;
        let key = content_sha256.unwrap_or(DELETED_REVISION_KEY);
        let changed = tx.execute(
            "INSERT INTO brownfield_refresh_queue(project_id,relative_path,content_revision_key,content_sha256,event_kind,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'pending',?6,?6) ON CONFLICT(project_id,relative_path,content_revision_key) DO UPDATE SET event_kind=excluded.event_kind,state=CASE WHEN COALESCE((SELECT content_sha256 FROM brownfield_source_cache WHERE project_id=excluded.project_id AND relative_path=excluded.relative_path),'deleted')<>excluded.content_revision_key THEN 'pending' ELSE brownfield_refresh_queue.state END,diagnostic=NULL,updated_at=excluded.updated_at",
            params![project_id, relative_path, key, content_sha256, event_kind, now],
        )?;
        Ok(changed > 0)
    }

    pub(super) fn ensure_brownfield_mapping_job_tx(
        tx: &Connection,
        project_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        if let Some(existing) = tx
            .query_row(
                "SELECT id FROM jobs WHERE project_id=?1 AND kind='brownfield_map' AND status IN ('pending','running','interrupted') AND cancellation_reason IS NULL ORDER BY created_at,id LIMIT 1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(existing);
        }
        let id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,project_id,job_class,idempotency_key) VALUES(?1,'brownfield_map','pending',0,?2,?2,?3,'background',?4)",
            params![id, now, project_id, format!("brownfield-map:{project_id}:{id}")],
        )?;
        Ok(id)
    }

    pub(super) fn ignored_brownfield_paths(
        db: &Connection,
        project_id: &str,
    ) -> Result<Vec<String>, PersistenceServiceError> {
        db.prepare(
            "SELECT relative_path FROM brownfield_frontier WHERE project_id=?1 AND status='ignored' ORDER BY relative_path",
        )?
        .query_map([project_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(super) fn record_brownfield_usage_tx(
        db: &Connection,
        project_id: &str,
        sources: &[SourceRevision],
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        for source in sources
            .iter()
            .filter(|source| source.project_id == project_id)
        {
            db.execute(
                "UPDATE brownfield_frontier SET recent_use_count=MIN(8,recent_use_count+1),last_used_at=?3,priority_reason='recent_user_task',updated_at=?3 WHERE project_id=?1 AND relative_path=?2 AND status<>'ignored'",
                params![project_id, source.relative_path.as_str(), now],
            )?;
        }
        Ok(())
    }

    pub(super) fn charge_brownfield_frontier_budget_tx(
        db: &Connection,
        project_id: &str,
        paths: &[RelativeSourcePath],
    ) -> Result<(), PersistenceServiceError> {
        for path in paths {
            db.execute(
                "UPDATE brownfield_frontier SET budget_consumed=budget_consumed+1,updated_at=?3 WHERE project_id=?1 AND relative_path=?2 AND status<>'ignored'",
                params![project_id, path.as_str(), Self::now_iso()],
            )?;
        }
        Ok(())
    }

    pub(super) fn recalculate_brownfield_frontier_tx(
        db: &Connection,
        project_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        db.execute(
            "INSERT OR IGNORE INTO brownfield_frontier(project_id,relative_path,status,content_sha256,priority_reason,priority_since,created_at,updated_at) SELECT project_id,relative_path,CASE WHEN status='mapped' THEN 'mapped' ELSE 'partial' END,content_sha256,'derived_from_cache',?2,?2,?2 FROM brownfield_source_cache WHERE project_id=?1",
            params![project_id, now],
        )?;
        db.execute(
            "UPDATE brownfield_frontier AS frontier SET in_degree=(SELECT COUNT(DISTINCT edge.source_path) FROM brownfield_edges edge LEFT JOIN brownfield_frontier source ON source.project_id=edge.project_id AND source.relative_path=edge.source_path WHERE edge.project_id=frontier.project_id AND edge.resolution='resolved' AND edge.destination_path=frontier.relative_path AND edge.source_path<>edge.destination_path AND COALESCE(source.status,'mapped')<>'ignored'),age_factor=MAX(0,CAST(julianday(?2)-julianday(frontier.priority_since) AS INTEGER)),priority_reason=CASE WHEN frontier.status='ignored' THEN 'ignored_by_user' WHEN frontier.recent_use_count>0 THEN 'recent_use='||frontier.recent_use_count||';in_degree='||(SELECT COUNT(DISTINCT edge.source_path) FROM brownfield_edges edge LEFT JOIN brownfield_frontier source ON source.project_id=edge.project_id AND source.relative_path=edge.source_path WHERE edge.project_id=frontier.project_id AND edge.resolution='resolved' AND edge.destination_path=frontier.relative_path AND edge.source_path<>edge.destination_path AND COALESCE(source.status,'mapped')<>'ignored')||';aging' ELSE 'in_degree='||(SELECT COUNT(DISTINCT edge.source_path) FROM brownfield_edges edge LEFT JOIN brownfield_frontier source ON source.project_id=edge.project_id AND source.relative_path=edge.source_path WHERE edge.project_id=frontier.project_id AND edge.resolution='resolved' AND edge.destination_path=frontier.relative_path AND edge.source_path<>edge.destination_path AND COALESCE(source.status,'mapped')<>'ignored')||';aging' END,updated_at=?2 WHERE frontier.project_id=?1",
            params![project_id, now],
        )?;
        Ok(())
    }
}

fn frontier_status(raw: &str) -> Option<BrownfieldFrontierStatus> {
    Some(match raw {
        "pending" => BrownfieldFrontierStatus::Pending,
        "in_progress" => BrownfieldFrontierStatus::InProgress,
        "partial" => BrownfieldFrontierStatus::Partial,
        "mapped" => BrownfieldFrontierStatus::Mapped,
        "stale" => BrownfieldFrontierStatus::Stale,
        "ignored" => BrownfieldFrontierStatus::Ignored,
        "failed" => BrownfieldFrontierStatus::Failed,
        _ => return None,
    })
}
