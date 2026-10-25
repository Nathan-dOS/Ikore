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
        51
    );
    for table in ["terminal_model_observations", "llm_model_observations"] {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get::<_, u32>(0),
            )
            .unwrap(),
            1,
            "migration V41 must create {table}"
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('terminal_model_observations') WHERE name='evidence_source'",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap(),
        1,
        "migration V42 must preserve the source of CLI model evidence"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('llm_attempts') WHERE name='observed_model'",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap(),
        1,
        "migration V41 must retain the primary observed model on each API attempt"
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
        51
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
        51
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

/// V35 widens `checkpoints.kind`/`purpose` to admit `path_confirmation` (the
/// ADR-022 clarification-loop preventive HITL gate, P5) via the same
/// `legacy_alter_table` rebuild V27 used; V36 adds
/// `specification_resolution_conversations` (P4). A populated V34 database
/// with one checkpoint of every pre-existing purpose, plus a clarification
/// question set, must migrate without losing rows, indexes or foreign keys,
/// and the widened CHECKs must admit exactly the new value.
#[test]
fn v34_to_v36_admits_path_confirmation_and_keeps_legacy_checkpoints_and_question_sets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(34))
        .run(&mut db)
        .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V34 fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version,state_version) VALUES('parked','p','Legacy','Specification','checkpoint',?1,?1,1,3,3)",[&now]).unwrap();
    for (id, purpose) in [
        ("review", "artifact_review"),
        ("clarify", "clarification"),
        ("fallback", "cli_fallback"),
        ("failure", "stage_failure"),
    ] {
        db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,generation,purpose) VALUES(?1,'parked','Specification','waiting',?2,?2,?3,1,3,1,?3)",params![id,now,purpose]).unwrap();
    }
    db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-legacy','p','specification','host-presented','Specification','now','now')",[]).unwrap();
    db.execute("INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES('set-legacy','clarify','parked',1,1,3,'conv-legacy','[]')",[]).unwrap();
    let legacy_indexes: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='checkpoints' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
    let purposes: Vec<String> = db
        .prepare("SELECT purpose FROM checkpoints ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        purposes,
        vec![
            "clarification".to_string(),
            "stage_failure".to_string(),
            "cli_fallback".to_string(),
            "artifact_review".to_string(),
        ]
    );
    let migrated_indexes: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='checkpoints' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for index in &legacy_indexes {
        assert!(
            migrated_indexes.contains(index),
            "{index} did not survive the V35 rebuild"
        );
    }
    // The widened CHECK admits exactly the new value, and only that one.
    db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,generation,purpose) VALUES('advisory','parked','Intake','waiting',?1,?1,'path_confirmation',1,3,2,'path_confirmation')",[&now]).expect("path_confirmation is admitted");
    db.execute(
        "UPDATE checkpoints SET purpose='made_up' WHERE id='advisory'",
        [],
    )
    .expect_err("unknown purposes stay refused");
    // The pre-existing question set survived and can now anchor a resolution conversation (P4/M5).
    db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-resolution','p','openai','gpt-fixture','Resolution','now','now')",[]).unwrap();
    db.execute("INSERT INTO specification_resolution_conversations(question_set_id,conversation_id,created_at) VALUES('set-legacy','conv-resolution','now')",[]).expect("resolution conversation links to the surviving question set");
    db.execute("INSERT INTO specification_resolution_conversations(question_set_id,conversation_id,created_at) VALUES('set-legacy','conv-resolution','now')",[]).expect_err("question_set_id is unique per resolution conversation");
    db.execute(
        "DELETE FROM specification_question_sets WHERE id='set-legacy'",
        [],
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM specification_resolution_conversations WHERE question_set_id='set-legacy'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0,
        "ON DELETE CASCADE must remove the resolution conversation link with its question set"
    );
}

/// V45 adds the typed failure and repair-note columns to `forge_turns`. A
/// populated V44 database with a turn that failed under the old raw-string
/// scheme must migrate with that turn intact, both columns empty, and the
/// JSON checks in force.
#[test]
fn v44_to_v45_keeps_forge_turns_and_adds_failure_columns() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(44))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V44 fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO forge_features(id,project_id,title,original_idea,brief_json,evaluation_json,created_at,updated_at) VALUES('f','p','UI update','Improve the interface','{}','{}','now','now')",[]).unwrap();
    db.execute("INSERT INTO forge_turns(id,feature_id,feature_revision,ordinal,request_id,status,user_message,error,created_at,updated_at) VALUES('t','f',1,1,'r','failed','Answers','Suggested dependency was not supplied by the host','now','now')",[]).unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    let (error, failure, notices): (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT error,failure_json,notices_json FROM forge_turns WHERE id='t'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(error, "Suggested dependency was not supplied by the host");
    assert!(
        failure.is_none() && notices.is_none(),
        "old rows stay untyped"
    );
    db.execute(
        "UPDATE forge_turns SET failure_json='{\"kind\":\"reply_unreadable\",\"technical\":\"x\"}',notices_json='[\"note\"]' WHERE id='t'",
        [],
    )
    .expect("well-formed JSON is stored");
    db.execute(
        "UPDATE forge_turns SET failure_json='not json' WHERE id='t'",
        [],
    )
    .expect_err("failure_json must be valid JSON");
    db.execute(
        "UPDATE forge_turns SET notices_json='not json' WHERE id='t'",
        [],
    )
    .expect_err("notices_json must be valid JSON");
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

/// V46 widens every backend CHECK to admit Antigravity's CLI. The rebuilt
/// tables must keep their rows, still reject an unknown backend, and leave
/// no dangling reference behind - a pulsation's history hangs off two of them.
#[test]
fn v45_to_v46_keeps_backend_rows_and_admits_antigravity() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(45))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V45 fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version) VALUES('pulse','p','Work','Code','running','now','now',2)",[]).unwrap();
    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','backend_agent','cli_process','opencode','high','[]',3,'now','now')",[]).unwrap();
    db.execute("INSERT INTO cli_backend_attempts(pulsation_id,agent_role,backend,error_code,diagnostic,created_at) VALUES('pulse','backend_agent','gemini','cli_rate_limited','503','now')",[]).unwrap();
    db.execute("INSERT INTO cli_model_catalog_snapshots(id,project_id,backend,adapter_contract_version,state,source,refreshed_at,created_at) VALUES('s','p','opencode',2,'ready','cli','now','now')",[]).unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    let (backend, version): (String, u32) = db
        .query_row(
            "SELECT backend,version FROM cli_agent_configs WHERE project_id='p' AND agent_role='backend_agent'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((backend.as_str(), version), ("opencode", 3));
    assert_eq!(
        db.query_row(
            "SELECT backend FROM cli_backend_attempts WHERE pulsation_id='pulse'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "gemini"
    );
    db.execute(
        "UPDATE cli_agent_configs SET backend='antigravity' WHERE project_id='p'",
        [],
    )
    .expect("Antigravity is a valid CLI backend");
    db.execute("INSERT INTO cli_backend_attempts(pulsation_id,agent_role,backend,error_code,diagnostic,created_at) VALUES('pulse','backend_agent','antigravity','cli_rate_limited','quota','now')",[])
        .expect("an Antigravity attempt can be recorded");
    db.execute("INSERT INTO cli_model_catalog_snapshots(id,project_id,backend,adapter_contract_version,state,source,refreshed_at,created_at) VALUES('a','p','antigravity',2,'ready','cli','now','now')",[])
        .expect("an Antigravity catalog snapshot can be stored");
    db.execute(
        "UPDATE cli_agent_configs SET backend='grok' WHERE project_id='p'",
        [],
    )
    .expect_err("an unknown backend is still rejected");
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

/// V47 rebuilds the role-constrained configuration, slot and run tables to
/// admit the Visualizer and the `review` run origin, and adds the storage for
/// its notes. Existing rows and the events hanging off a run must survive, an
/// unknown role must still be rejected, and no reference may dangle.
#[test]
fn v46_to_v47_keeps_terminal_rows_and_admits_the_visualizer() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(46))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V46 fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version) VALUES('pulse','p','Work','Code','running','now','now',4)",[]).unwrap();
    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','backend_agent','cli_process','claude','high','[]',2,'now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,created_at,updated_at) VALUES('slot','p','canonical','backend_agent','Backend','now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,origin,execution_mode,backend,pulsation_id,status,created_at,updated_at,slot_id) VALUES('run','p','canonical','backend_agent','pipeline','cli_process','claude','pulse','completed','now','now','slot')",[]).unwrap();
    db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at) VALUES('event','run',1,'stdout','stdout','done','now')",[]).unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    let (backend, version): (String, u32) = db
        .query_row(
            "SELECT backend,version FROM cli_agent_configs WHERE project_id='p' AND agent_role='backend_agent'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((backend.as_str(), version), ("claude", 2));
    let (slot, pulsation, events): (String, String, u32) = db
        .query_row(
            "SELECT r.slot_id,r.pulsation_id,(SELECT count(*) FROM terminal_events WHERE run_id=r.id) FROM terminal_runs r WHERE r.id='run'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (slot.as_str(), pulsation.as_str(), events),
        ("slot", "pulse", 1)
    );

    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','visualizer','api_direct','codex','default','[]',1,'now','now')",[])
        .expect("the Visualizer can be configured");
    db.execute("INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,created_at,updated_at) VALUES('visualizer-slot','p','canonical','visualizer','Visualizer','now','now')",[])
        .expect("the Visualizer has a canonical slot");
    db.execute("INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,origin,execution_mode,backend,pulsation_id,status,created_at,updated_at,slot_id) VALUES('review-run','p','canonical','visualizer','review','cli_process','codex','pulse','queued','now','now','visualizer-slot')",[])
        .expect("a Visualizer run carries the review origin");
    db.execute(
        "UPDATE cli_agent_configs SET agent_role='narrator' WHERE agent_role='visualizer'",
        [],
    )
    .expect_err("an unknown role is still rejected");
    db.execute(
        "UPDATE terminal_runs SET origin='narration' WHERE id='review-run'",
        [],
    )
    .expect_err("an unknown origin is still rejected");

    let columns = |table: &str| -> Vec<String> {
        db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(columns("checkpoints").contains(&"visualizer_brief_json".to_string()));
    assert_eq!(
        columns("checkpoint_visualizations"),
        [
            "checkpoint_id",
            "pulsation_id",
            "job_id",
            "state",
            "calls",
            "input_json",
            "brief_json",
            "diagnostic",
            "created_at",
            "updated_at"
        ]
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

/// V48 adds the plan-quota history (ADR-028). Its constraints reject values a collector should
/// never produce, and a periodic sample cannot carry a run id.
#[test]
fn v47_to_v48_adds_quota_history_with_validated_columns() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(47))
        .run(&mut db)
        .unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    let insert = |values: &str| {
        db.execute(
            &format!(
                "INSERT INTO quota_snapshots(backend,scope,window_minutes,used_percent,resets_at,source,plan,observed_at,kind,run_id,pulsation_id) VALUES({values})"
            ),
            [],
        )
    };
    insert("'codex',NULL,300,42.5,'2026-10-21T15:00:00.000Z','codex_app_server','plus','2026-10-21T12:00:00.000Z','sample',NULL,NULL")
        .expect("a periodic sample");
    insert("'claude',NULL,10080,0,NULL,'claude_endpoint',NULL,'2026-10-21T12:00:00.000Z','run_start','run','pulse'")
        .expect("a run-boundary sample");
    for rejected in [
        "'codex',NULL,300,100.5,NULL,'codex_app_server',NULL,'now','sample',NULL,NULL",
        "'codex',NULL,0,10,NULL,'codex_app_server',NULL,'now','sample',NULL,NULL",
        "'custom',NULL,300,10,NULL,'codex_app_server',NULL,'now','sample',NULL,NULL",
        "'codex',NULL,300,10,NULL,'web_scrape',NULL,'now','sample',NULL,NULL",
        "'codex',NULL,300,10,NULL,'codex_app_server',NULL,'now','sample','run',NULL",
        "'codex',NULL,300,10,NULL,'codex_app_server',NULL,'now','run_end',NULL,NULL",
    ] {
        insert(rejected).expect_err(rejected);
    }
}

#[test]
fn v48_to_v49_preserves_history_and_requires_new_sanitization_evidence() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("repo");
    fs::create_dir(&workspace).unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(48))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','Legacy',?1,'now','now')",[workspace.to_str().unwrap()]).unwrap();
    db.execute("INSERT INTO workspace_note_embeddings(id,workspace_path,relative_path,revision_hash,embedding_model,embedding_vector_json,updated_at) VALUES('vector',?1,'note.md','original-revision','fixture-model','[0.25,0.75]','now')",[workspace.to_str().unwrap()]).unwrap();
    db.execute("INSERT INTO embedding_sync_state(workspace_path,next_reconcile_at,embedding_model) VALUES(?1,'now','fixture-model')",[workspace.to_str().unwrap()]).unwrap();
    let original = "legacy password = synthetic-original-value";
    db.execute_batch("INSERT INTO provider_routes(id,name,created_at,updated_at) VALUES('legacy-route','Legacy route','now','now');").unwrap();
    db.execute("INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,agent_role,stage,strategy,mode,status,decision_json,visible_output,visible_output_complete,started_at) VALUES('legacy-api','g','p','legacy-route','backend_agent','Code','priority','off','completed','{}',?1,1,'now')",[original]).unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    for table in [
        "project_secret_redaction_exceptions",
        "secret_sanitization_receipts",
        "secret_sanitization_source_maps",
        "secret_publication_guards",
    ] {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            1
        );
    }
    let vector: (String,String,String) = db.query_row("SELECT revision_hash,embedding_vector_json,sanitization_policy_version FROM workspace_note_embeddings WHERE id='vector'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        vector,
        (
            "original-revision".into(),
            "[0.25,0.75]".into(),
            String::new()
        )
    );
    assert_eq!(
        db.query_row(
            "SELECT sanitization_policy_version FROM embedding_sync_state",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        ""
    );
    let legacy: (String,u64,String) = db.query_row("SELECT visible_output,output_redaction_count,output_redaction_categories_json FROM llm_route_executions WHERE id='legacy-api'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(legacy, (original.into(), 0, "[]".into()));
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM secret_sanitization_receipts",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );

    let store = hvc_core::sanitization::SanitizationStore::new(&path);
    assert_eq!(store.summary("p", None).unwrap().output_redaction_count, 0);
    for _ in 0..2 {
        store
            .record_input(
                "p",
                None,
                "new-call",
                hvc_core::secret_redactor::SANITIZATION_POLICY_VERSION,
                1,
                &["sensitive_variable".into()],
                40,
                1,
            )
            .unwrap();
    }
    let receipt: (u32, u64) = db
        .query_row(
            "SELECT COUNT(*),SUM(exception_count) FROM secret_sanitization_receipts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(receipt, (1, 0));
    drop(db);
    PersistenceService::initialize(root.path()).unwrap();
    let summary = store.summary("p", None).unwrap();
    assert_eq!(
        (
            summary.entries_examined,
            summary.input_redaction_count,
            summary.output_redaction_count
        ),
        (1, 1, 0)
    );
}

/// V50 rebuilds the role-constrained configuration, slot and run tables again
/// to admit the Notes Scribe and the `notes` run origin (ADR-029), and adds the
/// Companion notes and their Scribe turns. Existing rows and the events hanging
/// off a run must survive, unknown roles and origins must still be rejected, a
/// ready note must carry its text and a note may run only one turn at a time.
#[test]
fn v49_to_v50_admits_the_notes_scribe_and_adds_companion_notes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(49))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V49 fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version) VALUES('pulse','p','Work','Code','running','now','now',4)",[]).unwrap();
    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','visualizer','cli_process','claude','high','[]',2,'now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,created_at,updated_at) VALUES('slot','p','canonical','visualizer','Visualizer','now','now')",[]).unwrap();
    db.execute("INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,origin,execution_mode,backend,pulsation_id,status,created_at,updated_at,slot_id) VALUES('run','p','canonical','visualizer','review','cli_process','claude','pulse','completed','now','now','slot')",[]).unwrap();
    db.execute("INSERT INTO terminal_events(id,run_id,sequence,kind,stream,content,created_at) VALUES('event','run',1,'stdout','stdout','done','now')",[]).unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    let (backend, version): (String, u32) = db
        .query_row(
            "SELECT backend,version FROM cli_agent_configs WHERE project_id='p' AND agent_role='visualizer'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((backend.as_str(), version), ("claude", 2));
    let (slot, origin, events): (String, String, u32) = db
        .query_row(
            "SELECT r.slot_id,r.origin,(SELECT count(*) FROM terminal_events WHERE run_id=r.id) FROM terminal_runs r WHERE r.id='run'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (slot.as_str(), origin.as_str(), events),
        ("slot", "review", 1)
    );

    db.execute("INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,thinking_level,custom_args_json,version,created_at,updated_at) VALUES('p','notes_scribe','api_direct','codex','default','[]',1,'now','now')",[])
        .expect("the Notes Scribe can be configured");
    db.execute("INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,created_at,updated_at) VALUES('scribe-slot','p','canonical','notes_scribe','NotesScribe','now','now')",[])
        .expect("the Notes Scribe has a canonical slot");
    db.execute("INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,origin,execution_mode,backend,status,created_at,updated_at,slot_id) VALUES('notes-run','p','canonical','notes_scribe','notes','cli_process','codex','queued','now','now','scribe-slot')",[])
        .expect("a Scribe run carries the notes origin and no pulsation");
    db.execute(
        "UPDATE cli_agent_configs SET agent_role='narrator' WHERE agent_role='notes_scribe'",
        [],
    )
    .expect_err("an unknown role is still rejected");
    db.execute(
        "UPDATE terminal_runs SET origin='narration' WHERE id='notes-run'",
        [],
    )
    .expect_err("an unknown origin is still rejected");

    db.execute("INSERT INTO companion_notes(id,project_id,capture_request_id,raw_input,created_at,updated_at) VALUES('note','p','capture-1','Save button vanishes in dark mode','now','now')",[])
        .expect("a capture is stored before the Scribe answers");
    db.execute("INSERT INTO companion_notes(id,project_id,capture_request_id,raw_input,created_at,updated_at) VALUES('again','p','capture-1','Other text','now','now')",[])
        .expect_err("a capture request id is used once per project");
    db.execute(
        "UPDATE companion_notes SET status='ready' WHERE id='note'",
        [],
    )
    .expect_err("a ready note carries its text");
    db.execute("UPDATE companion_notes SET status='ready',title='Save button in dark mode',summary='The button disappears.',content_markdown='- Contrast',output_language='en',tags_json='[\"ui_ux\"]' WHERE id='note'", [])
        .expect("a structured note becomes ready");
    db.execute(
        "UPDATE companion_notes SET tags_json='{}' WHERE id='note'",
        [],
    )
    .expect_err("tags are a JSON array");
    db.execute("INSERT INTO companion_note_turns(id,note_id,note_revision,ordinal,request_id,kind,status,created_at,updated_at) VALUES('t1','note',1,1,'r1','structure','running','now','now')",[])
        .expect("a structuring turn");
    db.execute("INSERT INTO companion_note_turns(id,note_id,note_revision,ordinal,request_id,kind,status,user_message,created_at,updated_at) VALUES('t2','note',1,2,'r2','refine','running','Shorter','now','now')",[])
        .expect_err("a note runs one turn at a time");
    db.execute("INSERT INTO companion_note_turns(id,note_id,note_revision,ordinal,request_id,kind,status,created_at,updated_at) VALUES('t3','note',1,3,'r3','refine','failed','now','now')",[])
        .expect_err("a refinement carries the developer's message");
    db.execute(
        "UPDATE companion_note_turns SET status='completed' WHERE id='t1'",
        [],
    )
    .expect_err("a completed turn carries the reply");

    let columns = |table: &str| -> Vec<String> {
        db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        columns("companion_notes"),
        [
            "id",
            "project_id",
            "capture_request_id",
            "revision",
            "status",
            "source_type",
            "raw_input",
            "raw_redactions",
            "title",
            "summary",
            "content_markdown",
            "tags_json",
            "output_language",
            "created_at",
            "updated_at"
        ]
    );
    assert_eq!(
        columns("companion_note_turns"),
        [
            "id",
            "note_id",
            "note_revision",
            "ordinal",
            "request_id",
            "kind",
            "status",
            "user_message",
            "user_redactions",
            "assistant_message",
            "response_json",
            "failure_json",
            "notices_json",
            "calls",
            "created_at",
            "updated_at"
        ]
    );
    db.execute("DELETE FROM companion_notes WHERE id='note'", [])
        .unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM companion_note_turns", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        0,
        "turns go with their note"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}

/// V51 adds the Integrator's evidence boundary (ADR-032): the base each path
/// held before a pulsation first published it, the classified review inputs
/// of an Integrator execution and the decisions taken beyond the human
/// revision-round threshold. Nothing existing is rebuilt; every new row goes
/// with its pulsation, and the states are closed sets.
#[test]
fn v50_to_v51_adds_the_review_boundary_tables_without_rebuilding_existing_ones() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    cli_migrations::migrations::runner()
        .set_target(refinery::Target::Version(50))
        .run(&mut db)
        .unwrap();
    db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','V50 fixture','C:/fixture','now','now')",[]).unwrap();
    db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version) VALUES('pulse','p','Work','Integrator','checkpoint','now','now',4)",[]).unwrap();
    db.execute("INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,updated_at,content,producer_stage,input_revision) VALUES('artifact','pulse','src/lib.rs','code','src/lib.rs','a','now','now','fn a() {}\n','Code',1)",[]).unwrap();
    db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,generation,purpose) VALUES('review','pulse','Integrator','waiting','now','now','artifact_review',1,1,1,'artifact_review')",[]).unwrap();
    let before: Vec<String> = db
        .prepare(
            "SELECT sql FROM sqlite_master WHERE type='table' AND sql IS NOT NULL ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(db);

    PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        51
    );
    let after: Vec<String> = db
        .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND sql IS NOT NULL AND name NOT IN ('pulsation_file_bases','integrator_review_inputs','pulsation_convergence_decisions') ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(before, after, "no existing table was rebuilt");

    db.execute("INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,content,byte_len,first_artifact_id,captured_at) VALUES('pulse','src/lib.rs','src/lib.rs','present','b','fn b() {}\n',10,'artifact','now')",[])
        .expect("a stored base");
    db.execute("INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,first_artifact_id,captured_at) VALUES('pulse','src/new.rs','src/new.rs','absent','artifact','now')",[])
        .expect("an absent base has no hash");
    db.execute("INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,first_artifact_id,captured_at) VALUES('pulse','src/x.rs','src/x.rs','present','artifact','now')",[])
        .expect_err("a present base has a hash");
    db.execute("INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,content,reason,first_artifact_id,captured_at) VALUES('pulse','src/y.rs','src/y.rs','present_unstored','c','bytes','too_large','artifact','now')",[])
        .expect_err("an unstored base keeps no bytes");
    db.execute("INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,reason,first_artifact_id,captured_at) VALUES('pulse','src/z.rs','src/z.rs','present_unstored','c','guessed','artifact','now')",[])
        .expect_err("the reasons are a closed set");
    db.execute("INSERT INTO integrator_review_inputs(stage_execution_id,pulsation_id,input_revision,manifest_json,manifest_sha256,review_policy_state,gap_count,created_at) VALUES('missing','pulse',1,'{}','d','absent',0,'now')",[])
        .expect_err("review inputs belong to an existing execution");
    db.execute("INSERT INTO pulsation_convergence_decisions(id,pulsation_id,checkpoint_id,input_revision,stage,decision,rounds_at_decision,threshold,reason,created_at) VALUES('d1','pulse','review',1,'Integrator','replan',3,3,'The approach is wrong.','now')",[])
        .expect("a decision beyond the threshold");
    db.execute("INSERT INTO pulsation_convergence_decisions(id,pulsation_id,checkpoint_id,input_revision,stage,decision,rounds_at_decision,threshold,reason,created_at) VALUES('d2','pulse','review',1,'Integrator','continue_beyond_threshold',4,3,'Again.','now')",[])
        .expect_err("one decision per checkpoint");
    db.execute("DELETE FROM pulsation_convergence_decisions", [])
        .unwrap();
    for (decision, threshold, reason) in [
        ("skip_threshold", 3, "Why."),
        ("replan", 0, "Why."),
        ("replan", 11, "Why."),
        ("replan", 3, ""),
    ] {
        db.execute("INSERT INTO pulsation_convergence_decisions(id,pulsation_id,checkpoint_id,input_revision,stage,decision,rounds_at_decision,threshold,reason,created_at) VALUES('bad','pulse','review',1,'Integrator',?1,3,?2,?3,'now')",params![decision, threshold, reason])
            .expect_err("decisions, thresholds and reasons are bounded");
    }
    db.execute("INSERT INTO pulsation_convergence_decisions(id,pulsation_id,checkpoint_id,input_revision,stage,decision,rounds_at_decision,threshold,reason,created_at) VALUES('d1','pulse','review',1,'Integrator','replan',3,3,'The approach is wrong.','now')",[])
        .unwrap();
    db.execute("DELETE FROM pulsations WHERE id='pulse'", [])
        .unwrap();
    for table in ["pulsation_file_bases", "pulsation_convergence_decisions"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            0,
            "{table} goes with its pulsation"
        );
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, u32>(
            0
        ))
        .unwrap(),
        0
    );
}
