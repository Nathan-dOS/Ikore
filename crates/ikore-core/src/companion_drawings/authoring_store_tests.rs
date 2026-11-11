use super::*;
use crate::chat_service::ChatExecutionTargetRequest;
use crate::companion_drawings::authoring::{
    CanvasAuthoringBatchV1, CanvasAuthoringSession, CanvasCommandV1,
};
use crate::companion_drawings::semantics::{CanvasSemanticElementV1, CanvasSemanticKindV1};
use crate::companion_drawings::{CanvasElementKindV1, CanvasElementV1};
use crate::persistence_service::EnsureProjectForWorkspaceRequest;
use crate::terminals::CliBackend;
use rusqlite::params;

struct Fixture {
    _directory: tempfile::TempDir,
    persistence: PersistenceService,
    drawings: CompanionDrawingsService,
    store: CanvasAuthoringStore,
    project: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(&directory.path().join("data")).unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let project = persistence
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        let db = persistence.connection().unwrap();
        db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at,transport_kind,cli_backend,model_source) VALUES('conversation',?1,'codex','default','Conversation','now','now','cli','codex','default')", [&project]).unwrap();
        db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('assistant','conversation',0,'assistant','The interface calls the core service.','completed','now','now')", []).unwrap();
        let drawings = CompanionDrawingsService::new(persistence.clone());
        let store = CanvasAuthoringStore::new(drawings.clone());
        Self {
            _directory: directory,
            persistence,
            drawings,
            store,
            project,
        }
    }

    fn proposal(&self) -> CanvasOfferProposalV1 {
        CanvasOfferProposalV1 {
            profile: CanvasProfileV1::Architecture,
            language: "en".into(),
            title: "Architecture".into(),
            request_text: "May I explain this with Canvas?".into(),
        }
    }

    fn offer(&self) -> CanvasAuthoringRequestV1 {
        self.store
            .propose(
                &self.project,
                "conversation",
                "assistant",
                &self.proposal(),
                1_000,
            )
            .unwrap()
    }

    fn target(&self) -> CanvasCompanionTargetSnapshot {
        CanvasCompanionTargetSnapshot::capture_from_host(
            ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Codex,
                model: None,
                model_profile: None,
            },
            "default",
        )
        .unwrap()
    }

    fn snapshot(&self) -> Value {
        serde_json::json!({"transport":"cli", "backend":"codex", "model":"default", "profile":null})
    }

    fn admit(&self, request: &CanvasAuthoringRequestV1) -> CanvasAuthoringAdmissionV1 {
        self.store
            .admit(
                &self.project,
                "conversation",
                &request.request_id,
                request.request_revision,
                "approve",
                &self.target(),
                &self.snapshot(),
                1_000,
                1_900,
            )
            .unwrap()
    }

    fn candidate(&self, admission: &CanvasAuthoringAdmissionV1) -> CanvasAuthoringCandidate {
        let mut session = CanvasAuthoringSession::from_host_acceptance(
            &admission.context.job_id,
            admission.source.clone(),
            self.target(),
            admission.request.profile,
            &admission.request.title,
            1_000,
            admission.expires_at,
        )
        .unwrap();
        let mut element = CanvasElementV1::new(
            CanvasElementKindV1::Rectangle { round: true },
            10.,
            20.,
            100.,
            60.,
        );
        element.id = "component".into();
        element.label = "Core".into();
        let context = session.context().clone();
        session
            .apply(
                &context,
                &CanvasAuthoringBatchV1 {
                    schema_version: 1,
                    operation_id: "batch".into(),
                    expected_revision: 0,
                    commands: vec![CanvasCommandV1::UpsertElement {
                        element: Box::new(element),
                        semantic: Some(CanvasSemanticElementV1 {
                            element_id: "component".into(),
                            kind: CanvasSemanticKindV1::Component,
                            participant_id: None,
                            lane_id: None,
                            source_id: None,
                            target_id: None,
                        }),
                    }],
                },
                1_001,
            )
            .unwrap();
        let snapshot = session.inspect(&context, 1_001).unwrap();
        self.store
            .record_progress(&admission.context, &snapshot, 1_001)
            .unwrap();
        session
            .finish_candidate(&context, "finish", 1, 1_002)
            .unwrap()
    }
}

#[test]
fn canvas_store_proposal_is_persisted_idempotent_and_never_authorizes_or_creates_a_drawing() {
    let fixture = Fixture::new();
    let first = fixture.offer();
    let second = fixture.offer();
    assert_eq!(first, second);
    assert_eq!(
        first.state,
        CanvasAuthoringRequestStateV1::AwaitingAuthorization
    );
    assert!(first.job_id.is_none());
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
    assert_eq!(
        fixture
            .store
            .list(&fixture.project, "conversation")
            .unwrap(),
        vec![first]
    );
}

#[test]
fn canvas_store_source_requires_actual_project_conversation_completed_assistant_and_hash() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .store
            .propose(
                "foreign",
                "conversation",
                "assistant",
                &fixture.proposal(),
                1_000
            )
            .is_err()
    );
    assert!(
        fixture
            .store
            .propose(
                &fixture.project,
                "other",
                "assistant",
                &fixture.proposal(),
                1_000
            )
            .is_err()
    );
    let db = fixture.persistence.connection().unwrap();
    for (role, status) in [
        ("user", "completed"),
        ("assistant", "streaming"),
        ("assistant", "failed"),
    ] {
        db.execute(
            "UPDATE chat_messages SET role=?1,status=?2 WHERE id='assistant'",
            params![role, status],
        )
        .unwrap();
        assert!(
            fixture
                .store
                .propose(
                    &fixture.project,
                    "conversation",
                    "assistant",
                    &fixture.proposal(),
                    1_000
                )
                .is_err()
        );
    }
}

#[test]
fn canvas_store_admission_is_action_idempotent_and_only_one_job_is_admitted() {
    let fixture = Fixture::new();
    let request = fixture.offer();
    let first = fixture.admit(&request);
    let again = fixture.admit(&request);
    assert_eq!(first.context, again.context);
    assert_eq!(first.request.job_id, again.request.job_id);
    assert_eq!(first.request.request_revision, 2);
    assert_eq!(first.request.state, CanvasAuthoringRequestStateV1::Running);
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &request.request_id,
                1,
                "different_action",
                &fixture.target(),
                &fixture.snapshot(),
                1_000,
                1_900
            )
            .is_err()
    );
}

#[test]
fn canvas_store_admission_checks_ui_target_against_current_conversation_and_source() {
    let fixture = Fixture::new();
    let request = fixture.offer();
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE chat_conversations SET model_id_snapshot='different' WHERE id='conversation'",
            [],
        )
        .unwrap();
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &request.request_id,
                1,
                "approve",
                &fixture.target(),
                &fixture.snapshot(),
                1_000,
                1_900
            )
            .is_err()
    );
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE chat_conversations SET model_id_snapshot='default' WHERE id='conversation'",
            [],
        )
        .unwrap();
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET content='Replaced source' WHERE id='assistant'",
            [],
        )
        .unwrap();
    assert!(
        !fixture
            .store
            .get(&fixture.project, "conversation", &request.request_id)
            .unwrap()
            .source_available
    );
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &request.request_id,
                1,
                "approve",
                &fixture.target(),
                &fixture.snapshot(),
                1_000,
                1_900
            )
            .is_err()
    );
}

#[test]
fn canvas_store_decline_and_cancel_are_terminal_without_publishing() {
    let fixture = Fixture::new();
    let request = fixture.offer();
    let declined = fixture
        .store
        .decline(
            &fixture.project,
            "conversation",
            &request.request_id,
            1,
            1_001,
        )
        .unwrap();
    assert_eq!(declined.state, CanvasAuthoringRequestStateV1::Declined);
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &request.request_id,
                declined.request_revision,
                "approve",
                &fixture.target(),
                &fixture.snapshot(),
                1_002,
                1_900
            )
            .is_err()
    );
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
}

#[test]
fn canvas_store_staging_stays_hidden_and_has_an_independent_revision() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let request = fixture
        .store
        .get(
            &fixture.project,
            "conversation",
            &admission.context.request_id,
        )
        .unwrap();
    assert_eq!(request.request_revision, 2);
    assert_eq!(candidate.revision, 1);
    assert_eq!(request.state, CanvasAuthoringRequestStateV1::Running);
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
}

#[test]
fn canvas_store_publication_commits_drawing_provenance_ready_receipt_and_mirror_intent_once() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let ready = fixture
        .store
        .publish(&admission.context, &candidate, 1_002)
        .unwrap();
    assert_eq!(ready.state, CanvasAuthoringRequestStateV1::Ready);
    let receipt = ready.ready.as_ref().unwrap();
    let drawing = fixture
        .drawings
        .get(&fixture.project, &receipt.drawing_id)
        .unwrap();
    assert_eq!(drawing.revision, receipt.drawing_revision);
    assert_eq!(drawing.scene, candidate.scene);
    let again = fixture
        .store
        .publish(&admission.context, &candidate, 1_003)
        .unwrap();
    assert_eq!(ready, again);
    assert_eq!(fixture.drawings.list(&fixture.project).unwrap().len(), 1);
    let count: i64 = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn canvas_store_transaction_failure_rolls_back_drawing_and_journal() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    fixture.persistence.connection().unwrap().execute_batch("CREATE TRIGGER reject_canvas_ready BEFORE UPDATE OF state ON companion_canvas_requests WHEN NEW.state='ready' BEGIN SELECT RAISE(ABORT,'synthetic ready failure'); END;").unwrap();
    assert!(
        fixture
            .store
            .publish(&admission.context, &candidate, 1_002)
            .is_err()
    );
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
    let count: i64 = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM companion_drawing_sync_jobs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        fixture
            .store
            .get(
                &fixture.project,
                "conversation",
                &admission.context.request_id
            )
            .unwrap()
            .state,
        CanvasAuthoringRequestStateV1::Running
    );
}

#[test]
fn canvas_store_cancel_before_publication_blocks_candidate_but_ready_wins_late_cancel() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    fixture
        .store
        .cancel(
            &fixture.project,
            "conversation",
            &admission.context.request_id,
            2,
            1_002,
        )
        .unwrap();
    assert!(
        fixture
            .store
            .publish(&admission.context, &candidate, 1_003)
            .is_err()
    );
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let ready = fixture
        .store
        .publish(&admission.context, &candidate, 1_002)
        .unwrap();
    assert_eq!(
        fixture
            .store
            .cancel(
                &fixture.project,
                "conversation",
                &admission.context.request_id,
                2,
                1_003
            )
            .unwrap(),
        ready
    );
}

#[test]
fn canvas_store_deleted_ready_drawing_is_a_tombstone_and_cannot_be_recreated() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let ready = fixture
        .store
        .publish(&admission.context, &candidate, 1_002)
        .unwrap();
    let receipt = ready.ready.unwrap();
    fixture
        .drawings
        .delete(
            &fixture.project,
            &receipt.drawing_id,
            receipt.drawing_revision,
        )
        .unwrap();
    let tombstone = fixture
        .store
        .get(
            &fixture.project,
            "conversation",
            &admission.context.request_id,
        )
        .unwrap();
    assert_eq!(tombstone.state, CanvasAuthoringRequestStateV1::Ready);
    assert!(!tombstone.drawing_available);
    let replay = fixture
        .store
        .publish(&admission.context, &candidate, 1_003)
        .unwrap();
    assert!(!replay.drawing_available);
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
}

#[test]
fn canvas_store_worker_nonce_and_scope_are_checked_before_staging_or_publication() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let mut foreign = admission.context.clone();
    foreign.nonce = "wrong".into();
    assert!(fixture.store.publish(&foreign, &candidate, 1_002).is_err());
    assert!(
        fixture
            .store
            .fail(
                &foreign,
                &CanvasAuthoringFailureV1 {
                    code: "failed".into(),
                    message: "Fixture failure".into()
                },
                1_002
            )
            .is_err()
    );
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
}

#[test]
fn canvas_store_restart_interrupts_without_resuming_or_publishing_staging() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    fixture.candidate(&admission);
    let reopened = CanvasAuthoringStore::new(CompanionDrawingsService::new(
        PersistenceService::initialize(&fixture._directory.path().join("data")).unwrap(),
    ));
    assert_eq!(reopened.recover_interrupted(1_100).unwrap(), 1);
    assert_eq!(reopened.recover_interrupted(1_101).unwrap(), 0);
    assert_eq!(
        reopened
            .get(
                &fixture.project,
                "conversation",
                &admission.context.request_id
            )
            .unwrap()
            .state,
        CanvasAuthoringRequestStateV1::Interrupted
    );
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
    let staged: Option<String> = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row(
            "SELECT stage_scene_json FROM companion_canvas_requests WHERE id=?1",
            [&admission.context.request_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(staged.is_none());
}

#[test]
fn canvas_store_source_and_failure_are_sanitized_before_durable_storage() {
    let fixture = Fixture::new();
    let secret = "sk-proj-syntheticfixture0123456789";
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE chat_messages SET content=?1 WHERE id='assistant'",
            [format!("Architecture {secret}")],
        )
        .unwrap();
    let admission = fixture.admit(&fixture.offer());
    let failed = fixture
        .store
        .fail(
            &admission.context,
            &CanvasAuthoringFailureV1 {
                code: "fixture_failure".into(),
                message: format!("Failure {secret}"),
            },
            1_001,
        )
        .unwrap();
    assert!(!failed.failure.unwrap().message.contains(secret));
    let cached: String = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row(
            "SELECT source_explanation FROM companion_canvas_requests WHERE id=?1",
            [&admission.context.request_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!cached.contains(secret));
}

#[test]
fn canvas_store_one_running_job_per_conversation_and_project_delete_cascades() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    fixture.persistence.connection().unwrap().execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES('assistant_two','conversation',1,'assistant','Another explanation','completed','now','now')",[]).unwrap();
    let second = fixture
        .store
        .propose(
            &fixture.project,
            "conversation",
            "assistant_two",
            &fixture.proposal(),
            1_001,
        )
        .unwrap();
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &second.request_id,
                1,
                "second_action",
                &fixture.target(),
                &fixture.snapshot(),
                1_001,
                1_900
            )
            .is_err()
    );
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute("DELETE FROM projects WHERE id=?1", [&fixture.project])
        .unwrap();
    assert!(
        fixture
            .store
            .get(
                &fixture.project,
                "conversation",
                &admission.context.request_id
            )
            .is_err()
    );
    let count: i64 = fixture
        .persistence
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM companion_canvas_requests", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn canvas_store_publication_rechecks_source_expiry_and_exact_staged_candidate() {
    for fault in ["source", "expiry", "candidate"] {
        let fixture = Fixture::new();
        let admission = fixture.admit(&fixture.offer());
        let mut candidate = fixture.candidate(&admission);
        let now = if fault == "expiry" { 1_900 } else { 1_002 };
        if fault == "source" {
            fixture
                .persistence
                .connection()
                .unwrap()
                .execute(
                    "UPDATE chat_messages SET content='Changed' WHERE id='assistant'",
                    [],
                )
                .unwrap();
        }
        if fault == "candidate" {
            candidate.scene.elements[0].label = "Unrecorded change".into();
        }
        assert!(
            fixture
                .store
                .publish(&admission.context, &candidate, now)
                .is_err(),
            "{fault}"
        );
        assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
    }
}

#[test]
fn canvas_store_proposal_and_snapshot_bounds_reject_untrusted_or_secret_payloads() {
    let fixture = Fixture::new();
    let mut proposal = fixture.proposal();
    proposal.request_text = "x".repeat(4_001);
    assert!(
        fixture
            .store
            .propose(
                &fixture.project,
                "conversation",
                "assistant",
                &proposal,
                1_000
            )
            .is_err()
    );
    let request = fixture.offer();
    for snapshot in [
        serde_json::json!({"api_key":"sk-proj-syntheticfixture0123456789"}),
        serde_json::json!({"value":"x".repeat(32*1024+1)}),
    ] {
        assert!(
            fixture
                .store
                .admit(
                    &fixture.project,
                    "conversation",
                    &request.request_id,
                    1,
                    "approve",
                    &fixture.target(),
                    &snapshot,
                    1_000,
                    1_900
                )
                .is_err()
        );
    }
    let mut encoded = serde_json::to_value(fixture.proposal()).unwrap();
    encoded["approved"] = true.into();
    assert!(serde_json::from_value::<CanvasOfferProposalV1>(encoded).is_err());
}

#[test]
fn canvas_store_deleted_source_is_explicit_without_destroying_the_published_result() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let ready = fixture
        .store
        .publish(&admission.context, &candidate, 1_002)
        .unwrap();
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute("DELETE FROM chat_messages WHERE id='assistant'", [])
        .unwrap();
    let unavailable = fixture
        .store
        .get(
            &fixture.project,
            "conversation",
            &admission.context.request_id,
        )
        .unwrap();
    assert!(!unavailable.source_available);
    assert!(unavailable.drawing_available);
    assert_eq!(unavailable.ready, ready.ready);
    assert_eq!(
        fixture
            .store
            .publish(&admission.context, &candidate, 1_003)
            .unwrap()
            .ready,
        ready.ready
    );
}

#[test]
fn canvas_store_stored_target_metadata_is_revalidated_on_admission_replay() {
    let fixture = Fixture::new();
    let request = fixture.offer();
    fixture.admit(&request);
    fixture
        .persistence
        .connection()
        .unwrap()
        .execute(
            "UPDATE companion_canvas_requests SET target_snapshot_json=?1 WHERE id=?2",
            params![
                serde_json::json!({"api_key":"sk-proj-syntheticfixture0123456789"}).to_string(),
                request.request_id
            ],
        )
        .unwrap();
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &request.request_id,
                1,
                "approve",
                &fixture.target(),
                &fixture.snapshot(),
                1_001,
                1_900
            )
            .is_err()
    );
}

#[test]
fn canvas_store_mirror_failure_does_not_demote_an_authoritative_ready_result() {
    let fixture = Fixture::new();
    let admission = fixture.admit(&fixture.offer());
    let candidate = fixture.candidate(&admission);
    let ready = fixture
        .store
        .publish(&admission.context, &candidate, 1_002)
        .unwrap();
    // Simulate an unavailable workspace without permitting the writer to create a new root.
    std::fs::remove_dir(fixture._directory.path().join("workspace")).unwrap();
    let report = fixture.store.sync_project(&fixture.project).unwrap();
    assert!(report.failed > 0);
    assert_eq!(
        fixture
            .store
            .get(
                &fixture.project,
                "conversation",
                &admission.context.request_id
            )
            .unwrap(),
        ready
    );
    assert!(!fixture._directory.path().join("workspace").exists());
}

#[test]
fn canvas_store_admission_replay_cannot_replace_the_frozen_dispatch() {
    let fixture = Fixture::new();
    let request = fixture.offer();
    let admission = fixture.admit(&request);
    let altered_snapshot =
        serde_json::json!({"transport":"cli","backend":"claude","model":"default","profile":null});
    assert!(
        fixture
            .store
            .admit(
                &fixture.project,
                "conversation",
                &request.request_id,
                1,
                "approve",
                &fixture.target(),
                &altered_snapshot,
                1_001,
                1_900,
            )
            .is_err()
    );
    let original = fixture
        .store
        .admit(
            &fixture.project,
            "conversation",
            &request.request_id,
            1,
            "approve",
            &fixture.target(),
            &fixture.snapshot(),
            1_001,
            1_900,
        )
        .unwrap();
    assert_eq!(original.context, admission.context);
    assert_eq!(original.target_snapshot, admission.target_snapshot);
    assert!(fixture.drawings.list(&fixture.project).unwrap().is_empty());
}

#[test]
fn canvas_store_list_keeps_the_newest_thousand_offers_in_chronological_order() {
    let fixture = Fixture::new();
    let oldest = fixture.offer();
    let mut db = fixture.persistence.connection().unwrap();
    let transaction = db.transaction().unwrap();
    for ordinal in 1..=1_000 {
        let message_id = format!("source-{ordinal:04}");
        let request_id = format!("request-{ordinal:04}");
        transaction.execute(
            "INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at)
             VALUES(?1,'conversation',?2,'assistant','The interface calls the core service.','completed','now','now')",
            params![message_id, ordinal],
        ).unwrap();
        // Equal timestamps exercise deterministic insertion order at the retention boundary.
        transaction.execute(
            "INSERT INTO companion_canvas_requests(id,project_id,conversation_id,assistant_message_id,source_sha256,source_explanation,profile,language,title,request_text,state,created_at,updated_at)
             VALUES(?1,?2,'conversation',?3,?4,'The interface calls the core service.','architecture','en','Architecture','Use Canvas?','awaiting_authorization',1000,1000)",
            params![request_id, fixture.project, message_id, oldest.source_sha256],
        ).unwrap();
    }
    transaction.commit().unwrap();
    let requests = fixture
        .store
        .list(&fixture.project, "conversation")
        .unwrap();
    assert_eq!(requests.len(), 1_000);
    assert!(
        !requests
            .iter()
            .any(|request| request.request_id == oldest.request_id)
    );
    assert_eq!(requests.first().unwrap().request_id, "request-0001");
    assert_eq!(requests.last().unwrap().request_id, "request-1000");
    assert!(requests.iter().all(|request| request.source_available));
    assert!(
        requests
            .windows(2)
            .all(|pair| pair[0].created_at <= pair[1].created_at)
    );
}
