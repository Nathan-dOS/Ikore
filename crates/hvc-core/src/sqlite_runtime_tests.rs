use super::{Connection, PersistenceService, params};

#[test]
fn linked_sqlite_meets_the_wal_memory_disclosure_security_floor() {
    // RUSTSEC-2026-0459: older bundled engines can expose uninitialized memory
    // when a database and its sibling WAL disagree about their page size.
    assert!(
        rusqlite::version_number() >= 3_054_000,
        "SQLite {} predates the WAL page-tail initialization fix",
        rusqlite::version()
    );
    let db = Connection::open_in_memory().unwrap();
    let linked_version: String = db
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(linked_version, rusqlite::version());
}

#[test]
fn unsigned_revisions_round_trip_and_reject_negative_or_overflow_values() {
    let db = Connection::open_in_memory().unwrap();
    for value in [0_u64, 1, i64::MAX as u64] {
        let actual: u64 = db
            .query_row("SELECT ?1", [value], |row| row.get(0))
            .unwrap();
        assert_eq!(actual, value);
    }
    let position: usize = db
        .query_row("SELECT ?1", [42_usize], |row| row.get(0))
        .unwrap();
    assert_eq!(position, 42);
    assert!(matches!(
        db.query_row("SELECT -1", [], |row| row.get::<_, u64>(0)),
        Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))
    ));
    assert!(matches!(
        db.query_row("SELECT ?1", [u64::MAX], |row| row.get::<_, i64>(0)),
        Err(rusqlite::Error::ToSqlConversionFailure(_))
    ));
    assert!(matches!(
        db.query_row("SELECT ?1", [usize::MAX], |row| row.get::<_, i64>(0)),
        Err(rusqlite::Error::ToSqlConversionFailure(_))
    ));
}

#[test]
fn migrated_wal_databases_preserve_snapshots_rollback_and_reopen_at_page_size_limits() {
    for page_size in [512, 4096, 65536] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hard-vibe-code.db");
        let mut writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "page_size", page_size).unwrap();
        let journal: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE hvc_runtime_probe (id INTEGER PRIMARY KEY, content TEXT NOT NULL)",
            )
            .unwrap();
        let original = "draft \u{2014} a\u{e7}\u{e3}o \u{1f39b}\u{fe0f}\n".repeat(600);
        writer
            .execute("INSERT INTO hvc_runtime_probe VALUES (1, ?1)", [&original])
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let read = |db: &Connection| -> String {
            db.query_row(
                "SELECT content FROM hvc_runtime_probe WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(read(&reader), original);
        let updated = format!("{original}committed");
        writer
            .execute(
                "UPDATE hvc_runtime_probe SET content=?1 WHERE id=1",
                [&updated],
            )
            .unwrap();
        assert_eq!(read(&reader), original);
        reader.execute_batch("COMMIT").unwrap();
        assert_eq!(read(&reader), updated);
        {
            let transaction = writer.transaction().unwrap();
            transaction
                .execute(
                    "UPDATE hvc_runtime_probe SET content=?1 WHERE id=1",
                    params!["rolled back"],
                )
                .unwrap();
            transaction.rollback().unwrap();
        }
        assert_eq!(read(&reader), updated);
        // Exercise the application's real migration and checksum path while
        // committed pages remain in WAL, without changing its journal policy.
        let service = PersistenceService::initialize(root.path()).unwrap();
        let db = service.connection().unwrap();
        let versions: Vec<(i64, String)> = db
            .prepare("SELECT version, checksum FROM refinery_schema_history ORDER BY version")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(versions.last().unwrap().0, 65);
        assert_eq!(read(&db), updated);
        drop((db, service, reader, writer));
        let reopened = PersistenceService::initialize(root.path()).unwrap();
        let db = reopened.connection().unwrap();
        assert_eq!(read(&db), updated);
        let actual_page_size: i64 = db
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(actual_page_size, page_size);
        let after: Vec<(i64, String)> = db
            .prepare("SELECT version, checksum FROM refinery_schema_history ORDER BY version")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(after, versions);
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let broken_foreign_keys: i64 = db
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(broken_foreign_keys, 0);
    }
}
