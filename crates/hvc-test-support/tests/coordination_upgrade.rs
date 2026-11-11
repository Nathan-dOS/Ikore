use hvc_core::{
    orchestration::{BaseState, ConfirmBaseReconciliationRequest},
    persistence_service::{CancelPulsationJobsRequest, PersistenceService},
};
use hvc_test_support::{
    DatabaseVersion, IsolatedTestEnvironment, LegacyPulseLayout, PulsationFixture,
};
use rusqlite::Connection;

#[test]
fn v9_multiple_owners_open_ui_blocked_and_require_each_explicit_selection() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::MultipleActive);
    let service =
        PersistenceService::initialize(fixture.database_path().parent().unwrap()).unwrap();
    service.recover_interrupted_jobs().unwrap();
    let snapshot = service.get_project_coordination("project-a").unwrap();
    assert_eq!(snapshot.pipeline.base_state, BaseState::NeedsReview);
    assert_eq!(snapshot.owner_candidates, vec!["pulse-a", "pulse-b"]);
    assert!(snapshot.pipeline.active_pulsation_id.is_none());
    let request = ConfirmBaseReconciliationRequest {
        project_id: "project-a".into(),
        expected_base_epoch: snapshot.pipeline.base_epoch,
        expected_pipeline_state_version: snapshot.pipeline.version,
        explanation: "Retomar B primeiro; A preservada".into(),
        resume_pulsation_id: Some("pulse-b".into()),
        expected_review_token: snapshot.review_token,
    };
    service.confirm_base_reconciliation(request).unwrap();
    assert_eq!(
        service
            .get_hitl_checkpoint("pulse-a")
            .unwrap()
            .checkpoint_id,
        "checkpoint-pulse-a"
    );
    assert_eq!(
        service
            .get_hitl_checkpoint("pulse-b")
            .unwrap()
            .checkpoint_id,
        "checkpoint-pulse-b"
    );
    service
        .cancel_pulsation_jobs(CancelPulsationJobsRequest {
            pulsation_id: "pulse-b".into(),
        })
        .unwrap();
    let snapshot = service.get_project_coordination("project-a").unwrap();
    assert_eq!(snapshot.pipeline.base_state, BaseState::NeedsReview);
    assert_eq!(snapshot.owner_candidates, vec!["pulse-a"]);
    let db = fixture.connection();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM agent_run_outputs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT SUM(pipeline_version) FROM pulsations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        service
            .get_project_coordination("project-b")
            .unwrap()
            .pipeline
            .base_state,
        BaseState::Ready
    );
}

#[test]
fn v10_upgrade_backs_up_and_preserves_m1_checkpoint_identity_without_reexecution() {
    let env = IsolatedTestEnvironment::new();
    env.prepare_database(DatabaseVersion::new(10));
    let db = Connection::open(env.data_dir().join("ikore.db")).unwrap();
    db.execute_batch("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','P','fixture','t','t');
        INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version) VALUES('a','p','A','SPECIFICATION','checkpoint','t','t',1,2);
        INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at) VALUES('checkpoint-m1','a','SPECIFICATION','waiting','t','t');").unwrap();
    let service = PersistenceService::initialize(env.data_dir()).unwrap();
    service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        service
            .get_project_coordination("p")
            .unwrap()
            .pipeline
            .active_pulsation_id
            .as_deref(),
        Some("a")
    );
    assert_eq!(
        service.get_hitl_checkpoint("a").unwrap().checkpoint_id,
        "checkpoint-m1"
    );
    let backups = std::fs::read_dir(env.data_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|p| {
            p.file_name()
                .to_string_lossy()
                .starts_with("ikore.pre-v24.")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(backups[0].path()).unwrap();
    assert_eq!(
        backup
            .query_row(
                "SELECT MAX(version) FROM refinery_schema_history",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM integration_learning_intents",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
