use super::*;
use crate::orchestration::ExecutionFence;
use crate::persistence_service::PersistenceService;
use ikore_contracts::exchange::HostComponent;

fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(service.database_path()).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    db.execute_batch(
        "INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','Exchange','C:/p','now','now');
         INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('q','Other','C:/q','now','now');
         INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version,state_version) VALUES('pulse','p','Work','Code','running','now','now',4,3);
         INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key) VALUES('job','pulse','run_stage::Code','running','now','now','p',1,2,3,'job');",
    )
    .unwrap();
    (root, db)
}

fn request(project: &str, subject: &str, key: &str) -> AppendRequest {
    AppendRequest::host(
        StreamRef::new(project, StreamKind::Pulsation, subject),
        LogRecordKind::PulsationTransition,
        HostComponent::PipelineState,
        Some(PayloadRef {
            table: "pulse_events",
            id: format!("event-{key}"),
        }),
        key,
    )
}

fn items(db: &Connection) -> u32 {
    db.query_row("SELECT COUNT(*) FROM exchange_items", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn sequences_grow_by_one_per_stream_and_globally() {
    let (_root, db) = database();
    let first = append(&db, &request("p", "pulse", "k1")).unwrap();
    let second = append(&db, &request("p", "pulse", "k2")).unwrap();
    let other = append(&db, &request("p", "other", "k1")).unwrap();
    assert_eq!(first.disposition, Disposition::Recorded);
    assert_eq!((first.seq, first.next_seq), (1, 2));
    assert_eq!((second.seq, second.next_seq), (2, 3));
    assert_eq!(other.seq, 1, "each stream counts on its own");
    assert!(first.global_seq < second.global_seq && second.global_seq < other.global_seq);
    let stored: (String, String, Option<String>) = db
        .query_row(
            "SELECT trust,producer_kind,producer_ref FROM exchange_items WHERE global_seq=?1",
            [first.global_seq],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        ("host".into(), "host".into(), Some("pipeline/state".into()))
    );
}

#[test]
fn a_repeated_key_is_a_duplicate_that_writes_nothing() {
    let (_root, db) = database();
    let first = append(&db, &request("p", "pulse", "k1")).unwrap();
    let again = append(&db, &request("p", "pulse", "k1")).unwrap();
    assert_eq!(again.disposition, Disposition::Duplicate);
    assert_eq!((again.seq, again.global_seq), (first.seq, first.global_seq));
    assert_eq!(items(&db), 1);
    let next: u64 = db
        .query_row("SELECT next_seq FROM exchange_streams", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(next, 2, "a duplicate does not consume a sequence");
}

#[test]
fn a_moved_fence_writes_nothing_and_aborts_the_domain_write() {
    let (_root, db) = database();
    let mut fenced = request("p", "pulse", "k1");
    fenced.fence = Some(ExecutionFence {
        project_id: "p".into(),
        pulsation_id: Some("pulse".into()),
        input_revision: Some(1),
        job_id: "job".into(),
        generation: 2,
        state_version: 2, // the pulsation is at 3
    });
    let receipt = append(&db, &fenced).unwrap();
    assert_eq!(receipt.disposition, Disposition::Fenced);
    assert_eq!(items(&db), 0);

    let outcome: Result<(), ExchangeError> = atomically(&db, || {
        db.execute(
            "UPDATE pulsations SET feature_name='Changed' WHERE id='pulse'",
            [],
        )?;
        record(&db, &fenced)?;
        Ok(())
    });
    assert!(matches!(outcome, Err(ExchangeError::Fenced)));
    let name: String = db
        .query_row(
            "SELECT feature_name FROM pulsations WHERE id='pulse'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "Work", "the domain write rolled back with the append");

    fenced.fence.as_mut().unwrap().state_version = 3;
    assert_eq!(
        append(&db, &fenced).unwrap().disposition,
        Disposition::Recorded
    );
}

#[test]
fn incoherent_records_are_refused() {
    let (_root, db) = database();
    let mut spaced = request("p", "pulse", "has space");
    assert!(matches!(
        append(&db, &spaced),
        Err(ExchangeError::InvalidRecord(_))
    ));
    spaced.idempotency_key = "k".repeat(MAX_KEY_BYTES + 1);
    assert!(matches!(
        append(&db, &spaced),
        Err(ExchangeError::InvalidRecord(_))
    ));
    let mut wrong_stream = request("p", "pulse", "k1");
    wrong_stream.kind = LogRecordKind::NoteChanged;
    assert!(matches!(
        append(&db, &wrong_stream),
        Err(ExchangeError::InvalidRecord(_))
    ));
    let mut bad_digest = request("p", "pulse", "k1");
    bad_digest.content_sha256 = Some("ABC".into());
    assert!(matches!(
        append(&db, &bad_digest),
        Err(ExchangeError::InvalidRecord(_))
    ));
    assert_eq!(items(&db), 0);
}

#[test]
fn a_stream_out_of_step_with_its_items_fails_closed() {
    let (_root, db) = database();
    append(&db, &request("p", "pulse", "k1")).unwrap();
    db.execute("UPDATE exchange_streams SET next_seq=5", [])
        .unwrap();
    let error = append(&db, &request("p", "pulse", "k2")).unwrap_err();
    assert!(matches!(
        error,
        ExchangeError::SequenceGap {
            next_seq: 5,
            recorded: 1,
            ..
        }
    ));
    assert!(!error.retryable());
    assert_eq!(items(&db), 1);
}

#[test]
fn items_are_append_only() {
    let (_root, db) = database();
    append(&db, &request("p", "pulse", "k1")).unwrap();
    assert!(
        db.execute("UPDATE exchange_items SET kind='forged'", [])
            .is_err()
    );
}

#[test]
fn pages_follow_the_cursor_and_reset_when_too_far_behind() {
    let (_root, db) = database();
    for index in 0..5 {
        append(&db, &request("p", "pulse", &format!("k{index}"))).unwrap();
        append(&db, &request("q", "pulse-q", &format!("k{index}"))).unwrap();
    }
    let first = read_after(&db, "p", 0, 2).unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.more && !first.reset);
    assert!(first.items.iter().all(|item| item.project_id == "p"));
    assert_eq!(
        first.items[0].record_kind(),
        Some(LogRecordKind::PulsationTransition)
    );
    let rest = read_after(&db, "p", first.next_cursor, 10).unwrap();
    assert_eq!(rest.items.len(), 3);
    assert!(!rest.more);
    let stream = read_stream_after(&db, &first.items[0].stream_id, 3, 10).unwrap();
    assert_eq!(
        stream.iter().map(|item| item.seq).collect::<Vec<_>>(),
        vec![4, 5]
    );

    // A window of 20 pages of 1 item: 5 pending fit, 25 do not.
    assert!(!read_after(&db, "p", 0, 1).unwrap().reset);
    for index in 5..25 {
        append(&db, &request("p", "pulse", &format!("k{index}"))).unwrap();
    }
    let behind = read_after(&db, "p", 0, 1).unwrap();
    assert!(behind.reset && behind.items.is_empty());
    let newest = project_cursor(&db, "p").unwrap();
    let ahead = read_after(&db, "p", newest + 1_000, 10).unwrap();
    assert!(ahead.reset, "a cursor from another database is reset");
    let current = read_after(&db, "p", newest, 10).unwrap();
    assert!(current.items.is_empty() && !current.reset && !current.more);
}

#[test]
fn the_doorbell_cursor_names_each_projects_previous_item() {
    let (_root, db) = database();
    append(&db, &request("p", "pulse", "before")).unwrap();
    let mut cursor = DoorbellCursor::starting_now(&db).unwrap();
    assert!(
        cursor.poll(&db).unwrap().is_empty(),
        "it starts after the log"
    );
    let p1 = append(&db, &request("p", "pulse", "k1")).unwrap();
    let q1 = append(&db, &request("q", "pulse-q", "k1")).unwrap();
    let p2 = append(&db, &request("p", "pulse", "k2")).unwrap();
    let bells = cursor.poll(&db).unwrap();
    assert_eq!(bells.len(), 3);
    assert_eq!(bells[0].global_seq, p1.global_seq);
    assert_eq!(bells[0].previous_global_seq, p1.global_seq - 1);
    assert_eq!(
        (bells[1].project_id.as_str(), bells[1].previous_global_seq),
        ("q", 0)
    );
    assert_eq!(bells[2].previous_global_seq, p1.global_seq);
    assert_eq!(bells[1].global_seq, q1.global_seq);
    assert_eq!(bells[2].global_seq, p2.global_seq);
    assert!(cursor.poll(&db).unwrap().is_empty());
}

fn bell(project: &str, global_seq: i64, previous: i64) -> ExchangeDoorbellV1 {
    ExchangeDoorbellV1 {
        project_id: project.into(),
        stream_id: "stream".into(),
        stream_kind: StreamKind::Pulsation,
        subject_id: "pulse".into(),
        kind: LogRecordKind::PulsationTransition.as_str().into(),
        seq: 1,
        global_seq,
        previous_global_seq: previous,
    }
}

fn page(next_cursor: i64, more: bool) -> Page {
    Page {
        items: Vec::new(),
        next_cursor,
        more,
        reset: false,
    }
}

#[test]
fn the_follower_applies_in_order_and_reads_back_after_a_gap() {
    let mut follower = ExchangeFollower::new("p", 10);
    assert_eq!(
        follower.on_doorbell(&bell("p", 12, 10)),
        DoorbellAction::Apply
    );
    assert_eq!(follower.cursor(), 12);
    assert_eq!(
        follower.on_doorbell(&bell("q", 13, 0)),
        DoorbellAction::Ignore
    );
    assert_eq!(
        follower.on_doorbell(&bell("p", 12, 10)),
        DoorbellAction::Ignore
    );
    // 14 was missed.
    assert_eq!(
        follower.on_doorbell(&bell("p", 15, 14)),
        DoorbellAction::CatchUp(12)
    );
    assert!(follower.reading());
    assert_eq!(
        follower.on_doorbell(&bell("p", 16, 15)),
        DoorbellAction::Ignore
    );
    // The read returned through 15; 16 was announced meanwhile.
    assert_eq!(follower.on_page(&page(15, false)), PageAction::Continue(15));
    assert_eq!(follower.on_page(&page(16, false)), PageAction::Done);
    assert!(!follower.reading());
    assert_eq!(
        follower.on_doorbell(&bell("p", 17, 16)),
        DoorbellAction::Apply
    );
}

#[test]
fn a_lagged_broadcast_reads_once_and_a_reset_asks_for_a_snapshot() {
    let mut follower = ExchangeFollower::new("p", 3);
    assert_eq!(follower.on_lagged(), Some(3));
    assert_eq!(follower.on_lagged(), None, "one read at a time");
    assert_eq!(follower.on_page(&page(9, true)), PageAction::Continue(9));
    let reset = Page {
        reset: true,
        ..Page::default()
    };
    assert_eq!(follower.on_page(&reset), PageAction::Snapshot);
    follower.restart(40);
    assert_eq!((follower.cursor(), follower.reading()), (40, false));
    assert_eq!(follower.on_lagged(), Some(40));
    follower.on_read_failed();
    assert!(!follower.reading());
}

#[test]
fn record_kinds_belong_to_one_stream_kind_each() {
    for kind in LogRecordKind::ALL {
        assert_eq!(LogRecordKind::parse(kind.as_str()), Some(kind));
        assert!(StreamKind::ALL.contains(&kind.stream_kind()));
    }
    for kind in StreamKind::ALL {
        assert_eq!(StreamKind::parse(kind.as_str()), Some(kind));
    }
}
