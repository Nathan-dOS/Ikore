use super::*;
use crate::embedding_service::{EmbeddingError, EmbeddingFuture};
use crate::persistence_service::serial::tests::Fixture;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct RecordingSink(StdMutex<Vec<IndexingEvent>>);

impl IndexingEventSink for RecordingSink {
    fn emit(&self, event: &IndexingEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

impl RecordingSink {
    fn events(&self) -> Vec<IndexingEvent> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct Embeddings {
    calls: AtomicUsize,
    prompts: StdMutex<Vec<String>>,
    fail: AtomicBool,
    hold: AtomicBool,
    started: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl crate::embedding_service::EmbeddingGateway for Embeddings {
    fn embed<'a>(&'a self, _: &'a str, prompt: &'a str) -> EmbeddingFuture<'a> {
        Box::pin(async move {
            self.prompts.lock().unwrap().push(prompt.to_owned());
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            if self.hold.load(Ordering::SeqCst) {
                self.resume.notified().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                Err(EmbeddingError("offline fixture".into()))
            } else {
                Ok(vec![0.5, 0.8])
            }
        })
    }
}
fn fixture() -> (Fixture, Arc<Embeddings>) {
    let mut f = Fixture::new(PulsationStage::Code);
    let gateway = Arc::new(Embeddings::default());
    f.service.embedding_gateway = gateway.clone();
    fs::create_dir_all(f.workspace.path().join("knowledge/obsidian")).unwrap();
    let mut config = f.service.load_runtime_config().unwrap();
    config.embedding_enabled = true;
    f.service.persist_runtime_config(&config).unwrap();
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE jobs SET status='done' WHERE job_class='main' AND status='pending'",
            [],
        )
        .unwrap();
    f.service
        .connection()
        .unwrap()
        .execute("DELETE FROM runtime_slots WHERE job_class='main'", [])
        .unwrap();
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE artifact_publications SET state='cancelled' WHERE job_class='main' AND state IN ('pending','applying')",
            [],
        )
        .unwrap();
    (f, gateway)
}
fn write(f: &Fixture, body: &str) {
    fs::write(f.workspace.path().join("knowledge/obsidian/note.md"), body).unwrap();
}
async fn enqueue(f: &Fixture) -> ReindexWorkspaceEmbeddingsResponse {
    f.service
        .reindex_workspace_embeddings_for_workspace(
            f.workspace.path().to_str().unwrap(),
            ReindexWorkspaceEmbeddingsRequest { model: None },
        )
        .await
        .unwrap();
    f.service
        .reconcile_index_batch(
            f.workspace.path(),
            &f.service.load_runtime_config().unwrap().embedding_model,
            true,
            &CancellationToken::new(),
        )
        .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn delta_queue_coalesces_changes_deletions_and_model_switches() {
    let (f, gateway) = fixture();
    runtime().block_on(async {
        write(&f,"first");
        assert_eq!(enqueue(&f).await.queued_count,1);
        assert_eq!(gateway.calls.load(Ordering::SeqCst),0,"manual indexing only queues");
        assert_eq!(enqueue(&f).await.queued_count,0);
        write(&f,"second");
        assert_eq!(enqueue(&f).await.queued_count,1);
        f.service.process_indexing(&CancellationToken::new()).await.unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst),1);
        assert_eq!(f.count("workspace_note_embeddings"),1);
        assert_eq!(enqueue(&f).await.queued_count,0);
        let mut config=f.service.load_runtime_config().unwrap();
        config.embedding_model="fixture-new-model".into();
        f.service.persist_runtime_config(&config).unwrap();
        f.service.indexing_configuration_changed(&config).unwrap();
        let obsolete:i64=f.service.connection().unwrap().query_row("SELECT COUNT(*) FROM workspace_note_embeddings WHERE embedding_model<>'fixture-new-model'",[],|row|row.get(0)).unwrap();
        assert_eq!(obsolete,0,"model switch invalidates old vectors");
        f.service.process_indexing(&CancellationToken::new()).await.unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst),2);
        fs::remove_file(f.workspace.path().join("knowledge/obsidian/note.md")).unwrap();
        assert_eq!(enqueue(&f).await.queued_count,1);
        f.service.process_indexing(&CancellationToken::new()).await.unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst),2,"tombstones have no provider call");
        let db=f.service.connection().unwrap();
        let active:i64=db.query_row("SELECT COUNT(*) FROM workspace_note_embeddings WHERE embedding_model='fixture-new-model'",[],|r|r.get(0)).unwrap();
        assert_eq!(active,0);
    });
}

#[test]
fn embedding_sanitization_policy_invalidates_vectors_without_rewriting_source_hash() {
    let (f, gateway) = fixture();
    let raw = "# Synthetic\nOPENAI_API_KEY=sk-proj-syntheticabcdefghijklmnop\n";
    runtime().block_on(async {
        write(&f, raw);
        enqueue(&f).await;
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        assert!(!gateway.prompts.lock().unwrap()[0].contains("sk-proj-syntheticabcdefghijklmnop"));
        let db = f.service.connection().unwrap();
        db.execute(
            "UPDATE workspace_note_embeddings SET sanitization_policy_version='old-policy'",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE embedding_sync_state SET sanitization_policy_version='old-policy',dirty=0",
            [],
        )
        .unwrap();
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        let (hash, version): (String, String) = db
            .query_row(
                "SELECT revision_hash,sanitization_policy_version FROM workspace_note_embeddings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(hash, ContentSha256::of(raw.as_bytes()).as_str());
        assert_eq!(version, crate::secret_redactor::SANITIZATION_POLICY_VERSION);
    });
}

#[test]
fn late_vector_is_fenced_by_current_bytes_then_new_delta_can_run() {
    let (f, gateway) = fixture();
    gateway.hold.store(true, Ordering::SeqCst);
    runtime().block_on(async {
        write(&f, "before");
        enqueue(&f).await;
        let stop = CancellationToken::new();
        let work = f.service.process_indexing(&stop);
        let mutate = async {
            gateway.started.notified().await;
            write(&f, "after");
            gateway.resume.notify_one();
        };
        let (result, ()) = tokio::join!(work, mutate);
        result.unwrap();
        assert_eq!(f.count("workspace_note_embeddings"), 0);
        gateway.hold.store(false, Ordering::SeqCst);
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(f.count("workspace_note_embeddings"), 1);
        let hash: String = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT revision_hash FROM workspace_note_embeddings",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hash, ContentSha256::of(b"after").as_str());
    });
}

#[test]
fn unavailable_provider_has_no_automatic_retry_loop_and_manual_retry_is_coordinated() {
    let (f, gateway) = fixture();
    let sink = Arc::new(RecordingSink::default());
    f.service.attach_indexing_event_sink(sink.clone());
    gateway.fail.store(true, Ordering::SeqCst);
    runtime().block_on(async {
        write(&f, "content");
        enqueue(&f).await;
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        PersistenceService::mark_index_dirty(&f.service.connection().unwrap(), f.workspace.path())
            .unwrap();
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        assert!(
            sink.events()
                .iter()
                .any(|event| matches!(event, IndexingEvent::Failed { .. })),
            "an unreachable provider must surface as a Failed indexing event, not silence"
        );
        gateway.fail.store(false, Ordering::SeqCst);
        assert_eq!(enqueue(&f).await.queued_count, 1);
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        assert_eq!(f.state().status, PulsationStatus::Running);
        assert!(matches!(
            sink.events().last(),
            Some(IndexingEvent::Idle { .. })
        ));
    });
}

#[test]
fn indexing_events_progress_then_idle_exactly_once() {
    let (f, gateway) = fixture();
    let sink = Arc::new(RecordingSink::default());
    f.service.attach_indexing_event_sink(sink.clone());
    fs::write(f.workspace.path().join("knowledge/obsidian/note-a.md"), "a").unwrap();
    fs::write(f.workspace.path().join("knowledge/obsidian/note-b.md"), "b").unwrap();
    let expected_workspace_path = f
        .workspace
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    runtime().block_on(async {
        assert_eq!(enqueue(&f).await.queued_count, 2);
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            sink.events().last(),
            Some(IndexingEvent::Progress { remaining: 1, .. })
        ));
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        let events = sink.events();
        assert!(matches!(events.last(), Some(IndexingEvent::Idle { .. })));
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, IndexingEvent::Idle { .. }))
                .count(),
            1,
            "reaching Idle must only be reported once"
        );
        assert!(
            events
                .iter()
                .all(|event| event.workspace_path() == expected_workspace_path)
        );
        // Nothing left to do: re-running must not re-emit Idle (no queued/running deltas left
        // to recompute `remaining` from).
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            sink.events()
                .iter()
                .filter(|event| matches!(event, IndexingEvent::Idle { .. }))
                .count(),
            1
        );
    });
}

#[test]
fn disable_during_call_fences_response_and_reenable_resumes_same_hash() {
    let (f, gateway) = fixture();
    gateway.hold.store(true, Ordering::SeqCst);
    runtime().block_on(async {
        write(&f, "same bytes");
        enqueue(&f).await;
        let stop = CancellationToken::new();
        let work = f.service.process_indexing(&stop);
        let disable = async {
            gateway.started.notified().await;
            let mut config = f.service.load_runtime_config().unwrap();
            config.embedding_enabled = false;
            f.service.persist_runtime_config(&config).unwrap();
            f.service.indexing_configuration_changed(&config).unwrap();
        };
        let (result, ()) = tokio::join!(work, disable);
        result.unwrap();
        assert_eq!(f.count("workspace_note_embeddings"), 0);
        assert!(
            f.service
                .reindex_workspace_embeddings_for_workspace(
                    f.workspace.path().to_str().unwrap(),
                    ReindexWorkspaceEmbeddingsRequest { model: None }
                )
                .await
                .is_err()
        );
        gateway.hold.store(false, Ordering::SeqCst);
        let mut config = f.service.load_runtime_config().unwrap();
        config.embedding_enabled = true;
        f.service.persist_runtime_config(&config).unwrap();
        f.service.indexing_configuration_changed(&config).unwrap();
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(f.count("workspace_note_embeddings"), 1);
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn proposed_learning_reports_are_not_embedded_even_when_moved() {
    let (f, gateway) = fixture();
    write(
        &f,
        "---\ntype: learning_report\nstatus: proposed\n---\nproposed rule",
    );
    runtime().block_on(async {
        assert_eq!(enqueue(&f).await.queued_count, 0);
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    });
}
#[test]
fn manual_request_only_marks_intent_and_waits_for_supervisor_slot() {
    let (f, gateway) = fixture();
    write(&f, "note");
    runtime().block_on(async {
        let db = f.service.connection().unwrap();
        assert!(
            PersistenceService::reserve_slot(&db, JobClass::Background, "held", None, None)
                .unwrap()
        );
        f.service
            .reindex_workspace_embeddings_for_workspace(
                f.workspace.path().to_str().unwrap(),
                ReindexWorkspaceEmbeddingsRequest { model: None },
            )
            .await
            .unwrap();
        assert_eq!(f.count("embedding_deltas"), 0);
        assert!(
            !f.service
                .process_indexing(&CancellationToken::new())
                .await
                .unwrap()
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
        f.service.release_runtime_slot("held").unwrap();
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn component_note_changes_use_exact_embedding_deltas_and_delete_vectors() {
    let (f, gateway) = fixture();
    fs::create_dir_all(f.workspace.path().join("knowledge/obsidian/components")).unwrap();
    let relative = "knowledge/obsidian/components/runtime.md";
    fs::write(
        f.workspace.path().join(relative),
        "---\ntype: component\nmapping-status: validated\n---\n# Runtime\n",
    )
    .unwrap();
    runtime().block_on(async {
        f.service
            .enqueue_embedding_path_change(f.workspace.path(), relative)
            .unwrap();
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        let indexed: u32 = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM workspace_note_embeddings WHERE relative_path=?1",
                [relative],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);

        fs::remove_file(f.workspace.path().join(relative)).unwrap();
        f.service
            .enqueue_embedding_path_change(f.workspace.path(), relative)
            .unwrap();
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        let indexed: u32 = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM workspace_note_embeddings WHERE relative_path=?1",
                [relative],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 0);
    });
}

#[test]
fn pending_user_job_always_wins_over_embedding_maintenance() {
    let (f, gateway) = fixture();
    let sink = Arc::new(RecordingSink::default());
    f.service.attach_indexing_event_sink(sink.clone());
    write(&f, "queued note");
    runtime().block_on(async {
        enqueue(&f).await;
        let main = Uuid::new_v4().to_string();
        f.service
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,project_id,job_class,idempotency_key) VALUES(?1,'user_task','pending',0,?2,?2,?3,'main',?1)",
                params![main, PersistenceService::now_iso(), f.pulse.project_id],
            )
            .unwrap();
        assert!(
            !f.service
                .process_indexing(&CancellationToken::new())
                .await
                .unwrap()
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
        // A second tick while still paused must not repeat the same Paused event.
        assert!(
            !f.service
                .process_indexing(&CancellationToken::new())
                .await
                .unwrap()
        );
        let paused_events: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| matches!(event, IndexingEvent::Paused { remaining: 1, .. }))
            .collect();
        assert_eq!(
            paused_events.len(),
            1,
            "an unchanged pause must be deduped across supervisor ticks"
        );
        f.service
            .connection()
            .unwrap()
            .execute("UPDATE jobs SET status='done' WHERE id=?1", [&main])
            .unwrap();
        assert!(
            f.service
                .process_indexing(&CancellationToken::new())
                .await
                .unwrap()
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        // The pause→resume transition must always surface, even though the dedup key changed
        // from Paused to Idle rather than repeating an identical value.
        assert!(matches!(
            sink.events().last(),
            Some(IndexingEvent::Idle { .. })
        ));
    });
}
