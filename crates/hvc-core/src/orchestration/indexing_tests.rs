use super::*;
use crate::embedding_service::{EmbeddingError, EmbeddingFuture};
use crate::persistence_service::serial::tests::Fixture;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Embeddings {
    calls: AtomicUsize,
    fail: AtomicBool,
    hold: AtomicBool,
    started: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl crate::embedding_service::EmbeddingGateway for Embeddings {
    fn embed<'a>(&'a self, _: &'a str, _: &'a str) -> EmbeddingFuture<'a> {
        Box::pin(async move {
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
        gateway.fail.store(false, Ordering::SeqCst);
        assert_eq!(enqueue(&f).await.queued_count, 1);
        f.service
            .process_indexing(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        assert_eq!(f.state().status, PulsationStatus::Running);
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
