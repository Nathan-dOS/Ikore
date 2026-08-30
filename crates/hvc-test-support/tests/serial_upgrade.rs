use hvc_core::{
    orchestration::ContentSha256,
    persistence_service::{
        ApplyHitlDecisionRequest, CreatePulsationRequest, HitlDecision, PersistenceService,
        PulsationStage,
    },
};
use hvc_test_support::{LegacyPulseLayout, PulsationFixture};
use rusqlite::{Connection, params};
use std::fs;

#[test]
fn v9_upgrade_preserves_outputs_checkpoints_and_restorable_wal_backup() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::OneActive);
    let db = fixture.connection();
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    db.execute("INSERT INTO logs(id,level,message,created_at) VALUES(1,'info','WAL backup proof','2026-08-27')",[]).unwrap();
    let original: String = db
        .query_row(
            "SELECT raw_output FROM agent_run_outputs WHERE id='output-pulse-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let directory = fixture.database_path().parent().unwrap().to_owned();
    let service = PersistenceService::initialize(&directory).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        16
    );
    assert_eq!(
        db.query_row(
            "SELECT raw_output FROM agent_run_outputs WHERE id='output-pulse-a'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        original
    );
    assert_eq!(
        service
            .read_artifact_content("pulse-a", "artifact-pulse-a")
            .unwrap(),
        "# Legacy specification"
    );
    assert_eq!(
        db.query_row(
            "SELECT content_sha256 FROM artifacts WHERE id='artifact-pulse-a'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        ContentSha256::of(b"# Legacy specification").as_str()
    );
    assert_eq!(
        db.query_row(
            "SELECT active_pulsation_id FROM project_pipeline_state WHERE project_id='project-a'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "pulse-a"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM stage_acceptances", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let backups = fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|p| {
            p.file_name()
                .to_string_lossy()
                .starts_with("hard-vibe-code.pre-v13.")
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
        9
    );
    assert_eq!(
        backup
            .query_row("SELECT message FROM logs WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "WAL backup proof"
    );
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let c = service.get_hitl_checkpoint("pulse-a").unwrap();
    let updated = service
        .apply_hitl_decision(ApplyHitlDecisionRequest {
            pulsation_id: c.pulsation_id,
            checkpoint_id: c.checkpoint_id,
            expected_state_version: c.state_version,
            decision: HitlDecision::Approve,
            feedback: None,
        })
        .unwrap();
    assert_eq!(updated.pulsation.stage, PulsationStage::Contract);
    assert_eq!(
        db.query_row(
            "SELECT pipeline_version FROM pulsations WHERE id='pulse-a'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    service.recover_interrupted_jobs().unwrap();
    PersistenceService::initialize(&directory).unwrap();
    assert_eq!(
        fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|p| p
                .file_name()
                .to_string_lossy()
                .starts_with("hard-vibe-code.pre-v13."))
            .count(),
        1
    );
}

#[test]
fn fifo_is_project_scoped_immutable_and_legacy_multi_owner_is_diagnosed() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::MultipleActive);
    let service =
        PersistenceService::initialize(fixture.database_path().parent().unwrap()).unwrap();
    let db = fixture.connection();
    let rows=db.prepare("SELECT id,queue_sequence FROM pulsations WHERE project_id='project-a' ORDER BY queue_sequence").unwrap().query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    assert_eq!(
        rows,
        vec![
            ("pulse-a".into(), 1),
            ("pulse-b".into(), 2),
            ("pulse-c".into(), 3)
        ]
    );
    assert_eq!(
        db.query_row(
            "SELECT queue_sequence FROM pulsations WHERE id='pulse-other'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(
        db.execute(
            "UPDATE pulsations SET queue_sequence=20 WHERE id='pulse-a'",
            []
        )
        .is_err()
    );
    let (owner,reason):(Option<String>,String)=db.query_row("SELECT active_pulsation_id,blocking_reason FROM project_pipeline_state WHERE project_id='project-a'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(owner, None);
    assert_eq!(reason, "multiple_legacy_owners");
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE status='waiting'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let created = service
        .create_pulsation(CreatePulsationRequest {
            project_id: "project-a".into(),
            feature_name: "New v2 task".into(),
        })
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT queue_sequence,pipeline_version FROM pulsations WHERE id=?1",
            [created.pulsation.id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (4, 2)
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind LIKE '%Learn%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn ambiguous_legacy_content_is_not_invented_and_missing_checkpoint_is_repaired_once() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::OneActive);
    let db = fixture.connection();
    db.execute(
        "UPDATE artifacts SET content_sha256=?1 WHERE id='artifact-pulse-a'",
        [ContentSha256::of(b"unknown bytes").as_str()],
    )
    .unwrap();
    db.execute("DELETE FROM checkpoints WHERE pulsation_id='pulse-a'", [])
        .unwrap();
    let service =
        PersistenceService::initialize(fixture.database_path().parent().unwrap()).unwrap();
    assert!(
        service
            .read_artifact_content("pulse-a", "artifact-pulse-a")
            .is_err()
    );
    assert_eq!(
        db.query_row(
            "SELECT legacy_adaptation FROM artifacts WHERE id='artifact-pulse-a'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unavailable_or_ambiguous"
    );
    service.get_hitl_checkpoint("pulse-a").unwrap();
    PersistenceService::initialize(fixture.database_path().parent().unwrap()).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id='pulse-a'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn migration_preserves_conflicting_bindings_until_explicit_versioned_save() {
    use hvc_core::llm::{
        AgentRole, LlmErrorKind, LlmRepository, SqliteLlmRepository,
        UpsertProviderRouteBindingRequest,
    };
    let fixture = PulsationFixture::new(LegacyPulseLayout::TwoQueued);
    let db = fixture.connection();
    for route in ["route-a", "route-b"] {
        db.execute(
            "INSERT INTO provider_routes(id,name,created_at,updated_at) VALUES(?1,?1,'now','now')",
            [route],
        )
        .unwrap();
    }
    for (stage, route) in [("Specification", "route-a"), ("SPECIFICATION", "route-b")] {
        db.execute("INSERT INTO provider_route_bindings(project_id,agent_role,stage,route_id,created_at,updated_at) VALUES('project-a','task_specifier',?1,?2,'now','now')",params![stage,route]).unwrap();
    }
    let service =
        PersistenceService::initialize(fixture.database_path().parent().unwrap()).unwrap();
    let repo = SqliteLlmRepository::new(fixture.database_path());
    assert_eq!(
        repo.resolve_route("project-a", AgentRole::TaskSpecifier, "Specification")
            .unwrap_err()
            .kind,
        LlmErrorKind::RouteConflict
    );
    assert_eq!(
        service
            .list_provider_route_bindings()
            .unwrap()
            .bindings
            .len(),
        2
    );
    service
        .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
            project_id: "project-a".into(),
            agent_role: "task_specifier".into(),
            stage: "SPECIFICATION".into(),
            route_id: "route-b".into(),
            enabled: Some(true),
            version: Some(1),
        })
        .unwrap();
    assert_eq!(
        service
            .list_provider_route_bindings()
            .unwrap()
            .bindings
            .len(),
        1
    );
    assert_eq!(
        repo.resolve_route("project-a", AgentRole::TaskSpecifier, "Specification")
            .unwrap()
            .id,
        "route-b"
    );
}

#[test]
fn v12_upgrade_backs_up_and_never_replays_historical_learning_intents() {
    use hvc_test_support::{DatabaseVersion, IsolatedTestEnvironment};
    let env = IsolatedTestEnvironment::new();
    env.prepare_database(DatabaseVersion::new(12));
    let db = Connection::open(env.data_dir().join("hard-vibe-code.db")).unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('old-project','old',?1,'now','now')",[env.workspace_dir().to_str().unwrap()]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version,queue_sequence) VALUES('old-pulse','old-project','done','Integrator','done','now','now',2,1)",[]).unwrap();
    db.execute("INSERT INTO integration_learning_intents(pulsation_id,integration_epoch,report_version,state,created_at) VALUES('old-pulse',1,1,'unavailable','now')",[]).unwrap();
    let service = PersistenceService::initialize(env.data_dir()).unwrap();
    service.recover_interrupted_jobs().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM learning_runs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT state FROM integration_learning_intents", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "unavailable"
    );
    let backups = fs::read_dir(env.data_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|p| {
            p.file_name()
                .to_string_lossy()
                .starts_with("hard-vibe-code.pre-v13.")
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
        12
    );
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
