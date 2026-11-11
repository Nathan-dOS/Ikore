use super::*;
use crate::persistence_service::EnsureProjectForWorkspaceRequest;

struct Fixture {
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    service: PersistenceService,
    project: String,
}

impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = PersistenceService::initialize(data.path()).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        Self {
            _data: data,
            _workspace: workspace,
            service,
            project,
        }
    }

    fn conversation(&self, id: &str) {
        self.service.connection().unwrap().execute(
            "INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES(?1,?2,'fixture','fixture','Fixture','now','now')",
            params![id, self.project],
        ).unwrap();
    }
}

#[test]
fn dock_layout_and_drafts_survive_reload_without_creating_messages() {
    let f = Fixture::new();
    f.conversation("chat-a");
    f.conversation("chat-b");
    let layout = CompanionDockLayout {
        open: true,
        conversations: vec![
            CompanionDockItem {
                topic_id: "chat-a".into(),
                conversation_id: Some("chat-a".into()),
                decision_id: None,
                minimized: false,
            },
            CompanionDockItem {
                topic_id: "chat-b".into(),
                conversation_id: Some("chat-b".into()),
                decision_id: None,
                minimized: true,
            },
        ],
    };
    assert!(
        f.service
            .save_companion_dock(&f.project, 1, &layout)
            .unwrap()
    );
    let draft = CompanionDraft {
        topic_id: "chat-a".into(),
        conversation_id: Some("chat-a".into()),
        revision: 1,
        content: "Unsent 🦀\nsecond line".into(),
        decision_id: None,
        decision_state_version: None,
    };
    assert!(f.service.save_companion_draft(&f.project, &draft).unwrap());
    let loaded = f.service.load_companion_dock(&f.project).unwrap();
    assert_eq!(loaded.layout, layout);
    assert_eq!(loaded.layout_revision, 1);
    assert_eq!(loaded.drafts, vec![draft]);
    let messages: i64 = f
        .service
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM chat_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 0);
}

#[test]
fn delayed_draft_and_layout_writes_cannot_replace_newer_content() {
    let f = Fixture::new();
    f.conversation("chat-a");
    let mut draft = CompanionDraft {
        topic_id: "chat-a".into(),
        conversation_id: Some("chat-a".into()),
        revision: 2,
        content: "newer".into(),
        decision_id: Some("checkpoint:old".into()),
        decision_state_version: Some(5),
    };
    assert!(f.service.save_companion_draft(&f.project, &draft).unwrap());
    draft.revision = 1;
    draft.content = "older".into();
    assert!(!f.service.save_companion_draft(&f.project, &draft).unwrap());
    let newer = CompanionDockLayout {
        open: true,
        conversations: vec![],
    };
    assert!(
        f.service
            .save_companion_dock(&f.project, 2, &newer)
            .unwrap()
    );
    assert!(
        !f.service
            .save_companion_dock(&f.project, 1, &CompanionDockLayout::default())
            .unwrap()
    );
    let loaded = f.service.load_companion_dock(&f.project).unwrap();
    assert_eq!(loaded.drafts[0].content, "newer");
    assert_eq!(loaded.drafts[0].decision_state_version, Some(5));
    assert_eq!(loaded.layout, newer);
}

#[test]
fn dock_rejects_cross_project_and_oversized_drafts_and_duplicate_layout_entries() {
    let f = Fixture::new();
    f.conversation("chat-a");
    let mut draft = CompanionDraft {
        topic_id: "chat-a".into(),
        conversation_id: Some("chat-a".into()),
        revision: 1,
        content: "x".repeat(20_001),
        decision_id: None,
        decision_state_version: None,
    };
    assert!(f.service.save_companion_draft(&f.project, &draft).is_err());
    draft.content = "safe".into();
    assert!(
        f.service
            .save_companion_draft("another-project", &draft)
            .is_err()
    );
    let item = CompanionDockItem {
        topic_id: "chat-a".into(),
        conversation_id: Some("chat-a".into()),
        decision_id: None,
        minimized: false,
    };
    let duplicate = CompanionDockLayout {
        open: true,
        conversations: vec![item.clone(), item],
    };
    assert!(
        f.service
            .save_companion_dock(&f.project, 1, &duplicate)
            .is_err()
    );
    assert!(f.service.load_companion_dock("missing").is_err());
    draft.revision = u64::MAX;
    assert!(f.service.save_companion_draft(&f.project, &draft).is_err());
}

#[test]
fn clearing_a_sent_draft_leaves_a_tombstone_against_old_debounce_writes() {
    let f = Fixture::new();
    f.conversation("chat-a");
    let mut draft = CompanionDraft {
        topic_id: "chat-a".into(),
        conversation_id: Some("chat-a".into()),
        revision: 1,
        content: "Original question".into(),
        decision_id: None,
        decision_state_version: None,
    };
    f.service.save_companion_draft(&f.project, &draft).unwrap();
    let old = draft.clone();
    draft.revision = 2;
    draft.content.clear();
    f.service.save_companion_draft(&f.project, &draft).unwrap();
    assert!(!f.service.save_companion_draft(&f.project, &old).unwrap());
    assert!(
        f.service.load_companion_dock(&f.project).unwrap().drafts[0]
            .content
            .is_empty()
    );
}

#[test]
fn conversation_deletion_cascades_draft_and_filters_stale_layout() {
    let f = Fixture::new();
    f.conversation("chat-a");
    f.service
        .save_companion_draft(
            &f.project,
            &CompanionDraft {
                topic_id: "chat-a".into(),
                conversation_id: Some("chat-a".into()),
                revision: 1,
                content: "private draft".into(),
                decision_id: None,
                decision_state_version: None,
            },
        )
        .unwrap();
    f.service
        .save_companion_dock(
            &f.project,
            1,
            &CompanionDockLayout {
                open: true,
                conversations: vec![CompanionDockItem {
                    topic_id: "chat-a".into(),
                    conversation_id: Some("chat-a".into()),
                    decision_id: None,
                    minimized: false,
                }],
            },
        )
        .unwrap();
    f.service
        .connection()
        .unwrap()
        .execute("DELETE FROM chat_conversations WHERE id='chat-a'", [])
        .unwrap();
    let loaded = f.service.load_companion_dock(&f.project).unwrap();
    assert!(loaded.drafts.is_empty());
    assert!(loaded.layout.conversations.is_empty());
}

#[test]
fn decision_topic_and_answer_draft_do_not_require_a_chat_or_pending_decision() {
    let f = Fixture::new();
    let layout = CompanionDockLayout {
        open: true,
        conversations: vec![CompanionDockItem {
            topic_id: "decision:checkpoint:old".into(),
            conversation_id: None,
            decision_id: Some("checkpoint:old".into()),
            minimized: false,
        }],
    };
    f.service
        .save_companion_dock(&f.project, 1, &layout)
        .unwrap();
    let draft = CompanionDraft {
        topic_id: "decision:checkpoint:old:question:a".into(),
        conversation_id: None,
        revision: 1,
        content: "Keep my answer after the decision changes.".into(),
        decision_id: Some("checkpoint:old".into()),
        decision_state_version: Some(4),
    };
    f.service.save_companion_draft(&f.project, &draft).unwrap();
    let loaded = f.service.load_companion_dock(&f.project).unwrap();
    assert_eq!(loaded.layout, layout);
    assert_eq!(loaded.drafts, vec![draft]);
    assert_eq!(
        f.service
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM chat_conversations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
