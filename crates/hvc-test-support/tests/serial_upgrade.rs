use hvc_core::{
    orchestration::ContentSha256,
    persistence_service::{
        ApplyHitlDecisionRequest, CreatePulsationRequest, HitlDecision, PersistenceService,
        PersistenceServiceError, PulsationStage,
    },
};
use hvc_test_support::{LegacyPulseLayout, PulsationFixture};
use rusqlite::{Connection, params};
use std::fs;

mod cli_migrations {
    use refinery::embed_migrations;
    embed_migrations!("../hvc-core/migrations");
}

#[test]
fn v23_cli_and_companion_schema_has_no_credential_columns() {
    let root = tempfile::tempdir().unwrap();
    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(root.path().join("hard-vibe-code.db")).unwrap();
    let tables = [
        "chat_conversations",
        "chat_model_calls",
        "cli_agent_configs",
        "terminal_slots",
        "terminal_runs",
        "cli_model_catalog_snapshots",
        "cli_model_catalog_entries",
        "companion_agent_output_documents",
    ];
    let forbidden = [
        "api_key",
        "apikey",
        "credential",
        "password",
        "secret",
        "access_token",
        "refresh_token",
        "auth_token",
    ];

    for table in tables {
        let mut statement = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.is_empty(), "expected V23 table {table}");
        for column in columns {
            let normalized = column.to_ascii_lowercase();
            assert!(
                forbidden.iter().all(|term| !normalized.contains(term)),
                "V23 table {table} exposes forbidden credential column {column}"
            );
        }
    }
}

#[test]
fn v20_to_v24_backfills_slots_and_preserves_terminal_and_chat_history() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&db_path).unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(20))
        .run(&mut db)
        .unwrap();
    db.execute(
        "INSERT INTO projects(id,name,workspace_path,is_archived,created_at,updated_at)
         VALUES('project-cli','CLI','C:/fixture-cli',0,'2026-01-01','2026-01-01')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO cli_agent_configs(
           project_id,agent_role,execution_mode,backend,model,thinking_level,
           custom_args_json,custom_risk_ack,version,created_at,updated_at
         ) VALUES('project-cli','backend_agent','cli_process','codex','legacy-model','high',
                  '[]',0,3,'2026-01-01','2026-01-02')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO terminal_runs(
           id,project_id,slot_kind,agent_role,origin,execution_mode,backend,status,
           created_at,updated_at
         ) VALUES('run-cli','project-cli','canonical','backend_agent','manual',
                  'cli_process','codex','completed','2026-01-03','2026-01-03')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO terminal_events(id,run_id,sequence,kind,content,created_at)
         VALUES('event-cli','run-cli',1,'message','preserved','2026-01-03')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_connections(
           id,provider_id,display_name,credential_ref,default_model,enabled,state,
           max_concurrency,version,created_at,updated_at
         ) VALUES('connection-cli','openai','OpenAI fixture','fixture-ref','gpt-fixture',
                  1,'active',1,1,'2026-01-01','2026-01-01')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO chat_conversations(
           id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at
         ) VALUES('chat-cli','project-cli','connection-cli','openai','gpt-fixture','Fixture chat',
                  '2026-01-01','2026-01-01')",
        [],
    )
    .unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(db_path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        32
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM terminal_slots
             WHERE project_id='project-cli' AND kind='canonical' AND agent_role='backend_agent'",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT slot_id FROM terminal_runs WHERE id='run-cli'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "canonical:project-cli:backend_agent"
    );
    assert_eq!(
        db.query_row(
            "SELECT content FROM terminal_events WHERE id='event-cli'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "preserved"
    );
    let legacy_selection = db
        .query_row(
            "SELECT model,thinking_level,model_profile,model_source
             FROM cli_agent_configs
             WHERE project_id='project-cli' AND agent_role='backend_agent'",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(legacy_selection.0.as_deref(), Some("legacy-model"));
    assert_eq!(legacy_selection.1, "high");
    assert_eq!(legacy_selection.2, None);
    assert_eq!(legacy_selection.3, "manual");
    let chat_target = db
        .query_row(
            "SELECT transport_kind,connection_id,model_id_snapshot,cli_backend
             FROM chat_conversations WHERE id='chat-cli'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(chat_target.0, "api");
    assert_eq!(chat_target.1.as_deref(), Some("connection-cli"));
    assert_eq!(chat_target.2, "gpt-fixture");
    assert_eq!(chat_target.3, None);
}

#[test]
fn v1_to_v24_is_idempotent_and_creates_the_cli_and_companion_schema() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&db_path).unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(1))
        .run(&mut db)
        .unwrap();
    drop(db);
    PersistenceService::initialize(root.path()).unwrap();
    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(db_path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='table' AND name IN (
               'terminal_slots','terminal_runs','terminal_events',
               'cli_model_catalog_snapshots','cli_model_catalog_entries',
               'companion_agent_output_documents','companion_agent_output_fts'
             )",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        7
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM refinery_schema_history WHERE version=23",
            [],
            |row| row.get::<_, u32>(0)
        )
        .unwrap(),
        1
    );
}

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
        32
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('brownfield_symbols','brownfield_edges','brownfield_resolution_state')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    let edge_columns = db
        .prepare("PRAGMA table_info(brownfield_edges)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for required in [
        "reference_key",
        "normalized_reference_json",
        "destination_symbol_key",
        "destination_revision_sha256",
        "resolution_context_sha256",
    ] {
        assert!(edge_columns.iter().any(|column| column == required));
    }
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
                .starts_with("hard-vibe-code.pre-v24.")
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
                .starts_with("hard-vibe-code.pre-v24."))
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
                .starts_with("hard-vibe-code.pre-v24.")
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

#[test]
fn v23_upgrade_preserves_populated_agent_slots_and_legacy_pulsations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(23))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','Upgrade fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version) VALUES('legacy','p','Legacy work','Code','failed','now','now',2)",[]).unwrap();
    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','backend_agent','api_direct','codex','high','[]',8,'now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,position,archived,created_at,updated_at) VALUES('slot','p','canonical','backend_agent','Code',0,0,'now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_runs(id,project_id,slot_kind,slot_id,agent_role,origin,execution_mode,status,created_at,updated_at) VALUES('run','p','canonical','slot','backend_agent','manual','api_direct','completed','now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,content,created_at) VALUES('e','run',1,'message','Existing history','now')",[]).unwrap();
    drop(db);
    PersistenceService::initialize(root.path()).unwrap();
    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(db.query_row("SELECT version FROM cli_agent_configs WHERE project_id='p' AND agent_role='backend_agent'",[],|r|r.get::<_,u32>(0)).unwrap(),8);
    assert_eq!(
        db.query_row(
            "SELECT pipeline_version FROM pulsations WHERE id='legacy'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT content FROM terminal_events WHERE id='e'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Existing history"
    );
    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','feature_agent','api_direct','codex','default','[]',1,'now','now')",[]).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM forge_features", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0,
        "Legacy pulses must never become features implicitly"
    );
}

#[test]
fn line_ending_drift_in_an_applied_migration_is_reconciled() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&db_path).unwrap();
    let runner = cli_migrations::migrations::runner();
    runner.run(&mut db).unwrap();

    // Simulate the exact Windows CRLF-checkout bug: the checksum recorded for an
    // applied migration reflects the opposite line-ending convention of the sql
    // currently embedded in the binary.
    let migration = runner
        .get_migrations()
        .iter()
        .find(|m| m.version() == 1)
        .unwrap()
        .clone();
    let sql = migration.sql().unwrap();
    let alt_sql = if sql.contains("\r\n") {
        sql.replace("\r\n", "\n")
    } else {
        sql.replace('\n', "\r\n")
    };
    let alt_checksum = refinery::Migration::unapplied(&migration.to_string(), &alt_sql)
        .unwrap()
        .checksum()
        .to_string();
    db.execute(
        "UPDATE refinery_schema_history SET checksum=?1 WHERE version=1",
        params![alt_checksum],
    )
    .unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();

    let db = Connection::open(&db_path).unwrap();
    let stored: String = db
        .query_row(
            "SELECT checksum FROM refinery_schema_history WHERE version=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, migration.checksum().to_string());
}

#[test]
fn genuinely_altered_migration_checksum_is_never_silently_reconciled() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&db_path).unwrap();
    let runner = cli_migrations::migrations::runner();
    runner.run(&mut db).unwrap();

    // A checksum that still parses (refinery requires the stored value to be a valid u64)
    // but matches neither the real content nor either EOL variant of it: a genuine
    // edit/corruption, not a Windows checkout artifact. Reconciliation must leave this
    // untouched so refinery's own tamper detection still fires.
    let real_checksum = runner
        .get_migrations()
        .iter()
        .find(|m| m.version() == 1)
        .unwrap()
        .checksum();
    let bogus_checksum = real_checksum.wrapping_add(1).to_string();
    db.execute(
        "UPDATE refinery_schema_history SET checksum=?1 WHERE version=1",
        params![bogus_checksum],
    )
    .unwrap();
    drop(db);

    match PersistenceService::initialize(root.path()) {
        Err(error) => assert!(matches!(error, PersistenceServiceError::Migration(_))),
        Ok(_) => panic!("expected the genuinely altered checksum to fail initialization"),
    }
}

/// SC-T019: V32 rebuilds `pulsations` and `stage_failure_requests` to admit
/// pipeline v4 and the composed failure classes. A populated V31 database
/// with legacy pulsations in every state must migrate without losing rows,
/// indexes, the FIFO trigger or foreign keys, and the widened CHECKs must
/// admit exactly the new values.
#[test]
fn v31_to_v32_admits_pipeline_v4_and_keeps_legacy_rows_and_constraints() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(31))
        .run(&mut db)
        .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V31 fixture','C:/fixture','now','now')",[]).unwrap();
    for (id, status, version, sequence) in [
        ("queued", "planning", 2, 1),
        ("running", "running", 3, 2),
        ("parked", "checkpoint", 3, 3),
        ("finished", "done", 2, 4),
    ] {
        db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version,state_version) VALUES(?1,'p','Legacy','Code',?2,?4,?4,?5,?3,3)",params![id,status,version,now,sequence]).unwrap();
    }
    db.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key) VALUES('job','parked','run_stage::Code','interrupted',?1,?1,'p',1,2,3,'job')",[&now]).unwrap();
    db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('failure','parked','Code','waiting',?1,?1,'stage_failure',1,3,'job',2,'stage_failure')",[&now]).unwrap();
    db.execute("INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,revision_stage,error_code,diagnostic,created_at,updated_at) VALUES('failure','parked','job','Code','output_truncated',0,1,'Code','truncated','boom',?1,?1)",[&now]).unwrap();
    let legacy_indexes: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name IN ('pulsations','stage_failure_requests') AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        32
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    let states: Vec<(String, String, u8, i64)> = db
        .prepare("SELECT id,status,pipeline_version,queue_sequence FROM pulsations ORDER BY queue_sequence")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        states,
        vec![
            ("queued".into(), "planning".into(), 2, 1),
            ("running".into(), "running".into(), 3, 2),
            ("parked".into(), "checkpoint".into(), 3, 3),
            ("finished".into(), "done".into(), 2, 4),
        ]
    );
    let migrated_indexes: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name IN ('pulsations','stage_failure_requests') AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for index in &legacy_indexes {
        assert!(
            migrated_indexes.contains(index),
            "{index} did not survive the rebuild"
        );
    }
    // The FIFO sequence stays immutable after the rebuild.
    db.execute(
        "UPDATE pulsations SET queue_sequence=9 WHERE id='queued'",
        [],
    )
    .expect_err("pulse_sequence_immutable trigger must survive V32");
    // The legacy failure row is intact and its new columns are empty.
    let legacy: (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT failure_class,stage_execution_id,allowed_revision_targets_json FROM stage_failure_requests WHERE checkpoint_id='failure'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(legacy, ("output_truncated".into(), None, None));
    // The widened CHECKs admit exactly the new values.
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version) VALUES('v4','p','Composed','Intake','idle',?1,?1,5,4)",[&now]).expect("pipeline_version 4 is admitted");
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version) VALUES('v5','p','Unknown','Intake','idle',?1,?1,6,5)",[&now]).expect_err("pipeline_version 5 is refused");
    db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES('composed','parked','Code','waiting',?1,?1,'stage_failure',1,3,'job',3,'stage_failure')",[&now]).unwrap();
    db.execute(r#"INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,revision_stage,error_code,diagnostic,created_at,updated_at,allowed_revision_targets_json,remaining_budget_json) VALUES('composed','parked','job','Code','contract_conflict',0,1,'Code','contract_conflict','boom',?1,?1,'["Code","SPECIFICATION"]','{"extraInvocations":1}')"#,[&now]).expect("composed failure classes are admitted");
    db.execute(
        "UPDATE stage_failure_requests SET failure_class='made_up' WHERE checkpoint_id='composed'",
        [],
    )
    .expect_err("unknown failure classes stay refused");
    db.execute("UPDATE stage_failure_requests SET allowed_revision_targets_json='not json' WHERE checkpoint_id='composed'", [])
        .expect_err("revision targets must be JSON");
    for table in [
        "composition_policy_snapshots",
        "stage_budget_accounts",
        "stage_executions",
        "stage_work_units",
        "stage_unit_dependencies",
        "stage_unit_ownership",
        "stage_invocation_reservations",
        "stage_work_unit_attempts",
        "host_validation_runs",
        "project_composition_settings",
        "composition_specialty_overrides",
        "project_validation_profiles",
    ] {
        assert!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
            "V32 table {table} is missing"
        );
    }
    for column in [
        "stage_execution_id",
        "work_unit_id",
        "attempt_id",
        "executor_kind",
    ] {
        assert!(
            db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('token_usage_records') WHERE name=?1)", [column], |r| r.get::<_, bool>(0)).unwrap(),
            "token_usage_records.{column} is missing"
        );
    }
    assert!(
        db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('stage_acceptances') WHERE name='stage_execution_id')", [], |r| r.get::<_, bool>(0)).unwrap()
    );
}
