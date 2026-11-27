//! Measurement DDL only. The production schema belongs to slice 4c.
use super::{MODEL, Result, Value, json, percentile, scale, time};
use crate::sky_bench::hash;
use rusqlite::{Connection, params};
use std::{
    cell::Cell,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use std::{fs, path::Path, time::Instant};
const DDL: &str = r#"
CREATE TABLE constellations(id INTEGER PRIMARY KEY,path TEXT,type TEXT,title TEXT,hash TEXT,region TEXT);
CREATE TABLE stars(id INTEGER,constellation_id INTEGER,heading TEXT,kind TEXT,text TEXT,text_hash TEXT,state TEXT,author TEXT,originPulse TEXT,lastPulse TEXT,from_generation INTEGER,to_generation INTEGER,PRIMARY KEY(id,from_generation));
CREATE TABLE lines(source_constellation INTEGER,source_star INTEGER,target_constellation INTEGER,type TEXT);
CREATE TABLE star_coordinates(star_id INTEGER,role TEXT,ordinal INTEGER,path TEXT,symbol TEXT,resolved_symbol TEXT,fp TEXT,state TEXT,line INTEGER,verified_at_commit TEXT);
CREATE TABLE star_embeddings(id INTEGER PRIMARY KEY,model TEXT,hash TEXT,vector BLOB);
CREATE TABLE constellation_embeddings(id INTEGER PRIMARY KEY,model TEXT,hash TEXT,vector BLOB);
CREATE VIRTUAL TABLE stars_fts USING fts5(text);
CREATE TABLE sky_metrics(generation INTEGER PRIMARY KEY,project TEXT,branch TEXT,head TEXT,stars INTEGER);
"#;
thread_local! {static WAIT_US:Cell<u64>=const{Cell::new(0)};static TIMEOUTS:Cell<usize>=const{Cell::new(0)};}
fn busy(retries: i32) -> bool {
    if retries >= 100 {
        TIMEOUTS.with(|n| n.set(n.get() + 1));
        return false;
    }
    let t = Instant::now();
    thread::sleep(Duration::from_millis(1));
    WAIT_US.with(|n| n.set(n.get() + t.elapsed().as_micros() as u64));
    true
}
fn open(path: &Path) -> Result<Connection> {
    let mut db = Connection::open(path)?;
    db.busy_handler(Some(busy))?;
    Ok(db)
}
fn reset_wait() {
    WAIT_US.with(|c| c.set(0));
    TIMEOUTS.with(|c| c.set(0));
}
fn stats() -> (u64, usize) {
    (WAIT_US.with(Cell::get), TIMEOUTS.with(Cell::get))
}
fn is_busy(e: &rusqlite::Error) -> bool {
    e.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
}
fn seed(db: &mut Connection, n: usize) -> Result<()> {
    db.execute_batch(DDL)?;
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO sky_metrics VALUES(1,'parcel','main','fixture',?1)",
        [n],
    )?;
    let vector: Vec<_> = (0..768)
        .flat_map(|i| (i as f32 / 768.).to_le_bytes())
        .collect();
    for i in 0..n {
        let note = i / 8;
        if i % 8 == 0 {
            tx.execute(
                "INSERT INTO constellations VALUES(?1,?2,'feature',?2,'fixture','parcel')",
                params![note, format!("features/district{note}")],
            )?;
            tx.execute(
                "INSERT INTO constellation_embeddings VALUES(?1,?2,'fixture',?3)",
                params![note, MODEL, &vector],
            )?;
        }
        let body = scale::synthetic_text(note, i % 8);
        tx.execute("INSERT INTO stars VALUES(?1,?2,'Behavior','behavior',?3,?4,'verified','agent','pulse','pulse',1,NULL)",params![i,note,body,hash(body.as_bytes())])?;
        tx.execute(
            "INSERT INTO star_embeddings VALUES(?1,?2,'fixture',?3)",
            params![i, MODEL, &vector],
        )?;
        tx.execute(
            "INSERT INTO stars_fts(rowid,text) VALUES(?1,?2)",
            params![i, body],
        )?;
        tx.execute("INSERT INTO star_coordinates VALUES(?1,'primary',0,'src/ledger.rs',NULL,NULL,'record.units','verified',?1,'fixture')",[i])?;
        tx.execute(
            "INSERT INTO lines VALUES(?1,?2,?3,'link')",
            params![note, i, (note + 1) % n.div_ceil(8)],
        )?;
    }
    tx.commit()?;
    Ok(())
}
fn read(db: &Connection, id: usize, kind: usize) -> rusqlite::Result<()> {
    match kind {
        0 => {
            db.prepare_cached("SELECT rowid FROM stars_fts WHERE stars_fts MATCH ?1 LIMIT 10")?
                .query_map(["outbound AND reviews"], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
        }
        1 => {
            db.query_row(
                "SELECT text FROM stars WHERE id=?1 AND to_generation IS NULL",
                [id],
                |r| r.get::<_, String>(0),
            )?;
        }
        _ => {
            db.query_row(
                "SELECT vector FROM star_embeddings WHERE id=?1",
                [id],
                |r| r.get::<_, Vec<u8>>(0),
            )?;
        }
    }
    Ok(())
}
fn writer(db: &mut Connection, n: usize, round: usize) -> rusqlite::Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let count = if round % 4 == 3 { 2000 } else { 256 };
    let generation = round + 2;
    for i in 0..count {
        let id = (round * 2000 + i) % n;
        if count == 256 {
            tx.execute(
                "UPDATE star_embeddings SET hash=?2 WHERE id=?1",
                params![id, format!("batch{round}")],
            )?;
        } else {
            tx.execute(
                "UPDATE stars SET to_generation=?2 WHERE id=?1 AND to_generation IS NULL",
                params![id, generation],
            )?;
            tx.execute("INSERT INTO stars SELECT id,constellation_id,heading,kind,text,text_hash,state,author,originPulse,lastPulse,?2,NULL FROM stars WHERE id=?1 AND to_generation=?2",params![id,generation])?;
        }
    }
    tx.execute(
        "INSERT INTO sky_metrics VALUES(?1,'parcel','main','fixture',?2)",
        params![generation, n],
    )?;
    tx.commit()
}
fn sample(n: usize, mode: &str) -> Result<Value> {
    let directory = tempfile::tempdir()?;
    let sky = directory.path().join("sky");
    fs::create_dir(&sky)?;
    let path = sky.join("parcel.sqlite");
    let mut db = open(&path)?;
    db.pragma_update(None, "journal_mode", mode)?;
    seed(&mut db, n)?;
    let start = Arc::new(Barrier::new(16));
    let finished = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    for reader in 0..15 {
        let start = start.clone();
        let finished = finished.clone();
        let path = path.clone();
        readers.push(thread::spawn(move || -> Result<_> {
            let db = open(&path)?;
            let mut latency = Vec::new();
            let mut counter = 0;
            reset_wait();
            start.wait();
            while !finished.load(Ordering::Acquire) {
                let clock = Instant::now();
                let result = read(
                    &db,
                    (reader * 193 + counter * 41) % n,
                    (reader + counter) % 3,
                );
                match result {
                    Ok(()) => latency.push(time(clock)),
                    Err(e) if is_busy(&e) => (),
                    Err(e) => return Err(e.into()),
                }
                counter += 1;
                thread::sleep(Duration::from_millis(1));
            }
            Ok((latency, stats()))
        }));
    }
    reset_wait();
    start.wait();
    let begin = Instant::now();
    let mut admitted = Vec::new();
    let writes = (|| -> Result<()> {
        for round in 0..12 {
            thread::sleep(
                (begin + Duration::from_millis(200 * (round as u64 + 1)))
                    .saturating_duration_since(Instant::now()),
            );
            let clock = Instant::now();
            loop {
                match writer(&mut db, n, round) {
                    Ok(()) => break,
                    Err(e) if is_busy(&e) && clock.elapsed() < Duration::from_secs(10) => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            admitted.push(time(clock));
        }
        Ok(())
    })();
    finished.store(true, Ordering::Release);
    let (mut waits, mut timeouts) = stats();
    let mut raw = Vec::new();
    for handle in readers {
        let (xs, (w, b)) = handle.join().map_err(|_| "reader panicked")??;
        raw.extend(xs);
        waits += w;
        timeouts += b;
    }
    writes?;
    Ok(
        json!({"stars":n,"journal":mode,"read_count":raw.len(),"read_p50_ms":percentile(&raw,50),"read_p95_ms":percentile(&raw,95),"read_p99_ms":percentile(&raw,99),
        "busy_timeout_expirations":timeouts,"lock_wait_ms":waits as f64/1000.,"writer_ms":admitted,"read_ms":raw}),
    )
}
pub fn run() -> Result<Value> {
    let mut rows = Vec::new();
    let mut decisions = Vec::new();
    for (n, modes) in [(5000, ["DELETE", "WAL"]), (50000, ["WAL", "DELETE"])] {
        let a = sample(n, modes[0])?;
        let b = sample(n, modes[1])?;
        let (wal, delete) = if modes[0] == "WAL" {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let num = |v: &Value, k: &str| v[k].as_f64().unwrap();
        let improved = ["read_p95_ms", "lock_wait_ms"]
            .iter()
            .any(|k| num(delete, k) > 0. && num(wal, k) <= num(delete, k) * 0.8);
        let regression = [
            "read_p50_ms",
            "read_p95_ms",
            "read_p99_ms",
            "lock_wait_ms",
            "busy_timeout_expirations",
        ]
        .iter()
        .any(|k| num(wal, k) > num(delete, k));
        decisions.push(json!({"stars":n,"wal_qualifies":improved&&!regression}));
        rows.extend([a, b]);
    }
    Ok(
        json!({"preliminary":if decisions.iter().all(|r|r["wal_qualifies"]==true){"WAL"}else{"DELETE"},"decisions":decisions,"runs":rows,"final_slice":"4e"}),
    )
}
