//! Explicit removal of the files a failed pulsation left in the workspace.
//!
//! A failure is absorbing and never undoes publications by itself (see
//! `cancel_pulsation_jobs`: "files already published were not undone"). This
//! is the operator's way to undo them, under the same rule as every other host
//! write: human content is never discarded. Only a file the pulsation itself
//! created (`expected_sha256 IS NULL`) that still holds bytes the pulsation
//! wrote is removed. A file that existed before the pulsation, or that changed
//! after publication, is kept and reported; the base review stays the place to
//! decide about those.
use std::collections::{BTreeMap, BTreeSet};

use super::{
    Connection, HashMap, Path, PathBuf, PersistenceService, PersistenceServiceError,
    PulsationStatus, Uuid, coordination, fs, params,
};
use crate::orchestration::{
    CleanupDisposition, CleanupFailedPulsationFilesRequest, ContentSha256,
    FailedPulsationCleanupEntry, FailedPulsationCleanupPlan, FailedPulsationCleanupReport,
    publication,
};

struct PublicationRow {
    relative_path: String,
    expected_sha256: Option<String>,
    desired_sha256: String,
    state: String,
}

/// Everything the confirmation needs to redo the preview under the write lock.
struct ResolvedPlan {
    plan: FailedPulsationCleanupPlan,
    root: PathBuf,
    /// Absolute staging directories that exist and belong to this pulsation.
    candidate_dirs: Vec<PathBuf>,
    /// Bytes the pulsation wrote to each removable path (any of them counts).
    /// Ordered so the plan token is stable across previews.
    written_hashes: BTreeMap<String, BTreeSet<String>>,
}

impl PersistenceService {
    /// Dry run: what `cleanup_failed_pulsation_files` would remove and what it
    /// would keep. Reads the workspace; writes nothing.
    pub fn preview_failed_pulsation_cleanup(
        &self,
        pulsation_id: &str,
    ) -> Result<FailedPulsationCleanupPlan, PersistenceServiceError> {
        let pulse = Self::normalize_pulsation_id(pulsation_id)?;
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        let resolved = self.resolve_cleanup_plan(&tx, &pulse)?;
        tx.commit()?;
        Ok(resolved.plan)
    }

    /// Removes the files of the confirmed preview. Refuses when the pulsation
    /// is not terminal-failed, when the project still has a live executor or
    /// publication, or when any observed file changed since the preview.
    pub fn cleanup_failed_pulsation_files(
        &self,
        request: CleanupFailedPulsationFilesRequest,
    ) -> Result<FailedPulsationCleanupReport, PersistenceServiceError> {
        let pulse = Self::normalize_pulsation_id(&request.pulsation_id)?;
        let mut db = self.connection()?;
        // The same write coordinator serializes publications and cancellation.
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let resolved = self.resolve_cleanup_plan(&tx, &pulse)?;
        if resolved.plan.plan_token != request.expected_plan_token {
            return Err(coordination::invalid(
                "The files changed since the preview. Refresh the list and check again.",
            ));
        }
        let project_id = resolved.plan.project_id.clone();
        let busy: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_slots WHERE project_id=?1) OR EXISTS(SELECT 1 FROM jobs WHERE project_id=?1 AND status='running') OR EXISTS(SELECT 1 FROM artifact_publications WHERE project_id=?1 AND job_class='main' AND state='applying')",
            [&project_id],
            |r| r.get(0),
        )?;
        if busy {
            return Err(coordination::invalid(
                "Wait for the running or publishing unit to finish before cleaning up.",
            ));
        }
        let now = Self::now_iso();
        let root = resolved.root.as_path();
        let mut removed_files = Vec::new();
        let mut removed_directories = 0usize;
        let mut knowledge_changed = false;
        for entry in &resolved.plan.entries {
            match entry.disposition {
                CleanupDisposition::Remove => {
                    let written = resolved
                        .written_hashes
                        .get(&entry.relative_path.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_default();
                    // Re-observe immediately before deleting: external editors do not
                    // share the host lock, so the preview is optimistic.
                    match publication::observed_hash(root, &entry.relative_path)? {
                        Some(observed) if written.contains(observed.as_str()) => {}
                        _ => {
                            return Err(coordination::invalid(format!(
                                "`{}` changed during the cleanup; nothing was removed. Refresh the preview.",
                                entry.relative_path
                            )));
                        }
                    }
                    let target = publication::safe_target(root, &entry.relative_path, false)?;
                    fs::remove_file(&target)?;
                    removed_directories += prune_empty_parents(root, &target);
                    knowledge_changed |= is_knowledge_note(&entry.relative_path);
                    removed_files.push(entry.relative_path.clone());
                }
                CleanupDisposition::AlreadyAbsent => {}
                CleanupDisposition::KeepPreexisting
                | CleanupDisposition::KeepEdited
                | CleanupDisposition::KeepUnreadable => continue,
            }
            tx.execute(
                "UPDATE artifact_publications SET removed_at=?3 WHERE pulsation_id=?1 AND relative_path=?2 COLLATE NOCASE AND job_class='main' AND removed_at IS NULL",
                params![pulse, entry.relative_path, now],
            )?;
            // The effect is undone, so the base review no longer lists it.
            tx.execute(
                "UPDATE artifacts SET reconciled_at=COALESCE(reconciled_at,?3) WHERE pulsation_id=?1 AND relative_path=?2 COLLATE NOCASE",
                params![pulse, entry.relative_path, now],
            )?;
            // An absent file has no baseline hash, exactly as a missing file
            // adopted by `confirm_base_reconciliation`.
            tx.execute(
                "UPDATE project_file_baselines SET content_sha256=NULL,updated_at=?3 WHERE project_id=?1 AND relative_path=?2",
                params![project_id, entry.relative_path, now],
            )?;
        }
        let mut removed_candidate_dirs = 0usize;
        for dir in &resolved.candidate_dirs {
            if dir.exists() {
                fs::remove_dir_all(dir)?;
                removed_candidate_dirs += 1;
            }
        }
        if knowledge_changed {
            Self::mark_index_dirty(&tx, root)?;
        }
        let kept_files = resolved.plan.kept_files();
        let summary = serde_json::json!({
            "removedFiles": removed_files,
            "removedDirectories": removed_directories,
            "removedCandidateDirs": removed_candidate_dirs,
            "keptFiles": kept_files,
        });
        tx.execute(
            "INSERT INTO pulse_events(id,pulsation_id,event_type,from_status,to_status,message,created_at) VALUES(?1,?2,'files_cleaned','failed','failed',?3,?4)",
            params![Uuid::new_v4().to_string(), pulse, summary.to_string(), now],
        )?;
        tx.commit()?;
        if knowledge_changed {
            self.document_cache.invalidate(root);
            for path in &removed_files {
                if is_knowledge_note(path) {
                    self.enqueue_embedding_path_change(root, path)?;
                }
            }
            self.wake_scheduler();
        }
        Ok(FailedPulsationCleanupReport {
            pulsation_id: pulse,
            removed_files,
            removed_directories,
            removed_candidate_dirs,
            kept_files,
        })
    }

    fn resolve_cleanup_plan(
        &self,
        db: &Connection,
        pulse: &str,
    ) -> Result<ResolvedPlan, PersistenceServiceError> {
        let current = Self::find_pulsation_by_id(db, pulse)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".into()))?;
        if current.status != PulsationStatus::Failed {
            return Err(coordination::invalid(
                "Only a failed pulsation can have its files removed.",
            ));
        }
        let root: String = db.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [&current.project_id],
            |r| r.get(0),
        )?;
        let root = PathBuf::from(root);
        let rows = db
            .prepare(
                "SELECT relative_path,expected_sha256,desired_sha256,state FROM artifact_publications WHERE pulsation_id=?1 AND job_class='main' AND removed_at IS NULL ORDER BY relative_path COLLATE NOCASE,rowid",
            )?
            .query_map([pulse], |r| {
                Ok(PublicationRow {
                    relative_path: r.get(0)?,
                    expected_sha256: r.get(1)?,
                    desired_sha256: r.get(2)?,
                    state: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // One decision per path, listed by path; within a path the rows keep
        // insertion order, so retries of the same stage republishing it come
        // after the first publication (which decides whether the file was created).
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<PublicationRow>> = HashMap::new();
        for row in rows {
            let key = row.relative_path.to_ascii_lowercase();
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(row);
        }
        let mut entries = Vec::with_capacity(order.len());
        let mut written_hashes = BTreeMap::new();
        for key in order {
            let group = &groups[&key];
            let relative_path = group[0].relative_path.clone();
            let created = group[0].expected_sha256.is_none();
            let applied = group.iter().any(|row| row.state == "applied");
            // 'conflict' never replaced the target; 'pending' never started.
            let written: BTreeSet<String> = group
                .iter()
                .filter(|row| matches!(row.state.as_str(), "applied" | "applying" | "cancelled"))
                .map(|row| row.desired_sha256.clone())
                .collect();
            let (disposition, detail) = match publication::observed_hash(&root, &relative_path) {
                Err(error) if applied => {
                    (CleanupDisposition::KeepUnreadable, Some(error.to_string()))
                }
                Err(_) => continue,
                Ok(None) if applied => (CleanupDisposition::AlreadyAbsent, None),
                Ok(None) => continue,
                Ok(Some(_)) if !created && applied => (CleanupDisposition::KeepPreexisting, None),
                Ok(Some(_)) if !created => continue,
                Ok(Some(observed)) if written.contains(observed.as_str()) => {
                    (CleanupDisposition::Remove, None)
                }
                Ok(Some(_)) if applied => (CleanupDisposition::KeepEdited, None),
                Ok(Some(_)) => continue,
            };
            if disposition.removes() {
                written_hashes.insert(key, written);
            }
            entries.push(FailedPulsationCleanupEntry {
                relative_path,
                disposition,
                detail,
            });
        }
        let mut candidate_dirs = Vec::new();
        if let Some(data_dir) = self.database_path().parent() {
            let candidates_root = data_dir.join("stage_candidates");
            let mut ids = db
                .prepare("SELECT id FROM stage_executions WHERE pulsation_id=?1 ORDER BY id")?
                .query_map([pulse], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            // The Integrator pre-check materializes its baseline under the pulsation id.
            ids.push(pulse.to_owned());
            for id in ids {
                let dir = candidates_root.join(&id);
                if dir.is_dir() {
                    candidate_dirs.push(dir);
                }
            }
        }
        let candidate_names: Vec<String> = candidate_dirs
            .iter()
            .filter_map(|dir| dir.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        let bytes = serde_json::to_vec(&(
            pulse,
            &current.updated_at,
            &entries,
            &candidate_names,
            &written_hashes,
        ))
        .map_err(|e| coordination::invalid(e.to_string()))?;
        let plan_token = ContentSha256::of(&bytes).as_str().to_owned();
        Ok(ResolvedPlan {
            plan: FailedPulsationCleanupPlan {
                pulsation_id: pulse.to_owned(),
                project_id: current.project_id,
                entries,
                candidate_dirs: candidate_names,
                plan_token,
            },
            root,
            candidate_dirs,
            written_hashes,
        })
    }
}

fn is_knowledge_note(relative_path: &str) -> bool {
    relative_path.starts_with("knowledge/obsidian/")
        && Path::new(relative_path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// Removes directories left empty by the deletion, stopping at the workspace
/// root or at the first directory that still has content. Nothing else is
/// ever removed, so a pre-existing directory only disappears if it is empty.
fn prune_empty_parents(root: &Path, removed: &Path) -> usize {
    let Ok(root) = root.canonicalize() else {
        return 0;
    };
    let mut pruned = 0;
    let mut current = removed.parent();
    while let Some(dir) = current {
        if dir == root || !dir.starts_with(&root) {
            break;
        }
        let empty = fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none());
        if !empty || fs::remove_dir(dir).is_err() {
            break;
        }
        pruned += 1;
        current = dir.parent();
    }
    pruned
}

#[cfg(test)]
mod tests {
    use crate::persistence_service::PulsationStage;

    use super::*;
    use crate::agent_runtime::GeneratedArtifact;
    use crate::orchestration::BaseState;
    use crate::persistence_service::CancelPulsationJobsRequest;
    use crate::persistence_service::serial::tests::Fixture;

    /// Code publishes without a checkpoint, so the files land on disk before
    /// the user cancels the pulsation on the next stage.
    fn failed_after_publishing(artifacts: Vec<GeneratedArtifact>) -> Fixture {
        let f = Fixture::new(PulsationStage::Code);
        f.accept(&f.output_artifacts("advance", artifacts)).unwrap();
        f.service.reconcile_publications().unwrap();
        f.service
            .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: f.pulse.id.clone(),
            })
            .unwrap();
        assert_eq!(f.state().status, PulsationStatus::Failed);
        f
    }

    fn artifact(path: &str, content: &str) -> GeneratedArtifact {
        GeneratedArtifact {
            name: path.to_owned(),
            kind: "stage_output".into(),
            relative_path: Some(path.to_owned()),
            content: content.to_owned(),
        }
    }

    fn preview(f: &Fixture) -> FailedPulsationCleanupPlan {
        f.service
            .preview_failed_pulsation_cleanup(&f.pulse.id)
            .unwrap()
    }

    fn confirm(f: &Fixture, plan: &FailedPulsationCleanupPlan) -> FailedPulsationCleanupReport {
        f.service
            .cleanup_failed_pulsation_files(CleanupFailedPulsationFilesRequest {
                pulsation_id: f.pulse.id.clone(),
                expected_plan_token: plan.plan_token.clone(),
            })
            .unwrap()
    }

    fn coordination(f: &Fixture) -> crate::orchestration::ProjectCoordinationSnapshot {
        f.service
            .get_project_coordination(&f.pulse.project_id)
            .unwrap()
    }

    #[test]
    fn removes_only_files_the_pulsation_created_and_prunes_empty_directories() {
        let f = failed_after_publishing(vec![
            artifact("src/generated/feature.rs", "pub fn feature() {}\n"),
            artifact("docs/feature.md", "# Feature\n"),
        ]);
        let root = f.workspace.path();
        assert!(root.join("src/generated/feature.rs").exists());
        assert_eq!(coordination(&f).pipeline.base_state, BaseState::NeedsReview);
        assert_eq!(coordination(&f).publications.len(), 2);

        let plan = preview(&f);
        assert_eq!(plan.removable_files(), 2);
        assert_eq!(plan.kept_files(), 0);
        assert!(!plan.is_noop());
        // The preview is a dry run.
        assert!(root.join("src/generated/feature.rs").exists());

        let report = confirm(&f, &plan);
        assert_eq!(
            report.removed_files,
            vec!["docs/feature.md", "src/generated/feature.rs"],
            "listed by path, independent of publication order"
        );
        assert!(!root.join("src/generated/feature.rs").exists());
        assert!(!root.join("src/generated").exists(), "empty parent pruned");
        assert!(!root.join("src").exists(), "pruning stops only at the root");
        assert!(root.exists());
        assert_eq!(report.removed_directories, 3);

        let db = f.service.connection().unwrap();
        let closed: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM artifact_publications WHERE pulsation_id=?1 AND removed_at IS NOT NULL",
                [&f.pulse.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(closed, 2);
        let baselines: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM project_file_baselines WHERE project_id=?1 AND content_sha256 IS NOT NULL",
                [&f.pulse.project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(baselines, 0);
        let events: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM pulse_events WHERE pulsation_id=?1 AND event_type='files_cleaned'",
                [&f.pulse.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 1);
        // Undone effects leave the base review; adopting the base stays explicit.
        let snapshot = coordination(&f);
        assert!(snapshot.publications.is_empty());
        assert_eq!(snapshot.pipeline.base_state, BaseState::NeedsReview);
        assert_eq!(f.state().status, PulsationStatus::Failed);
        // A second preview finds nothing left to do.
        assert!(preview(&f).is_noop());
    }

    #[test]
    fn keeps_preexisting_and_edited_files_and_reports_them() {
        let mut f = Fixture::new(PulsationStage::Code);
        fs::write(f.workspace.path().join("existing.rs"), "human\n").unwrap();
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO project_file_baselines(project_id,relative_path,content_sha256,updated_at) VALUES(?1,'existing.rs',?2,'2026-01-01T00:00:00Z')",
            params![
                f.pulse.project_id,
                ContentSha256::of(b"human\n").as_str()
            ],
        )
        .unwrap();
        // The handoff carries the publication bases, so the stage overwrites
        // `existing.rs` against the bytes it observed at admission.
        let fence = PersistenceService::execution_fence(&db, &f.job.id).unwrap();
        f.handoff = f
            .service
            .load_stage_handoff(&fence, PulsationStage::Code, 16000)
            .unwrap();
        drop(db);
        f.accept(&f.output_artifacts(
            "advance",
            vec![
                artifact("existing.rs", "overwritten\n"),
                artifact("edited.rs", "generated\n"),
                artifact("fresh.rs", "generated\n"),
            ],
        ))
        .unwrap();
        f.service.reconcile_publications().unwrap();
        f.service
            .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: f.pulse.id.clone(),
            })
            .unwrap();
        fs::write(f.workspace.path().join("edited.rs"), "generated + human\n").unwrap();

        let plan = preview(&f);
        let by_path = |path: &str| {
            plan.entries
                .iter()
                .find(|e| e.relative_path == path)
                .map(|e| e.disposition)
                .unwrap()
        };
        assert_eq!(by_path("existing.rs"), CleanupDisposition::KeepPreexisting);
        assert_eq!(by_path("edited.rs"), CleanupDisposition::KeepEdited);
        assert_eq!(by_path("fresh.rs"), CleanupDisposition::Remove);
        assert_eq!(plan.kept_files(), 2);

        let report = confirm(&f, &plan);
        assert_eq!(report.removed_files, vec!["fresh.rs"]);
        assert_eq!(report.kept_files, 2);
        assert_eq!(
            fs::read_to_string(f.workspace.path().join("existing.rs")).unwrap(),
            "overwritten\n"
        );
        assert_eq!(
            fs::read_to_string(f.workspace.path().join("edited.rs")).unwrap(),
            "generated + human\n"
        );
        // Kept files stay in the base review for the human decision (the
        // review lists artifacts by id, so compare as a set).
        let mut listed: Vec<String> = coordination(&f)
            .publications
            .into_iter()
            .map(|p| p.relative_path)
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["edited.rs", "existing.rs"]);
    }

    #[test]
    fn refuses_stale_previews_live_pulsations_and_busy_projects() {
        let live = Fixture::new(PulsationStage::Code);
        assert!(
            live.service
                .preview_failed_pulsation_cleanup(&live.pulse.id)
                .is_err(),
            "a running pulsation has nothing to clean"
        );

        let f = failed_after_publishing(vec![artifact("out.rs", "generated\n")]);
        let plan = preview(&f);
        // The file changed between preview and confirmation: nothing is removed.
        fs::write(f.workspace.path().join("out.rs"), "edited\n").unwrap();
        let error = f
            .service
            .cleanup_failed_pulsation_files(CleanupFailedPulsationFilesRequest {
                pulsation_id: f.pulse.id.clone(),
                expected_plan_token: plan.plan_token.clone(),
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("changed since the preview"),
            "{error}"
        );
        assert!(f.workspace.path().join("out.rs").exists());

        fs::write(f.workspace.path().join("out.rs"), "generated\n").unwrap();
        let plan = preview(&f);
        assert_eq!(plan.removable_files(), 1);
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO runtime_slots(job_class,token,project_id,acquired_at) VALUES('main','busy',?1,?2)",
            params![f.pulse.project_id, PersistenceService::now_iso()],
        )
        .unwrap();
        let error = f
            .service
            .cleanup_failed_pulsation_files(CleanupFailedPulsationFilesRequest {
                pulsation_id: f.pulse.id.clone(),
                expected_plan_token: plan.plan_token.clone(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("Wait for"), "{error}");
        assert!(f.workspace.path().join("out.rs").exists());
        db.execute("DELETE FROM runtime_slots WHERE token='busy'", [])
            .unwrap();
        confirm(&f, &plan);
        assert!(!f.workspace.path().join("out.rs").exists());
    }

    #[test]
    fn removes_disposable_stage_candidates_of_the_pulsation_only() {
        let f = failed_after_publishing(vec![]);
        let data_dir = f.service.database_path().parent().unwrap().to_path_buf();
        let mine = data_dir.join("stage_candidates").join(&f.pulse.id);
        let cache = data_dir.join("stage_candidates").join("cache");
        fs::create_dir_all(mine.join("integrator")).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(mine.join("integrator/manifest.json"), "[]").unwrap();
        let plan = preview(&f);
        assert_eq!(plan.candidate_dirs, vec![f.pulse.id.clone()]);
        assert!(plan.entries.is_empty());
        let report = confirm(&f, &plan);
        assert_eq!(report.removed_candidate_dirs, 1);
        assert!(!mine.exists());
        assert!(cache.exists(), "the shared build cache is never touched");
    }
}
