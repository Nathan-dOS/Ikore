//! Temporary read-only audit of feature tasks. Delete after use.
//!
//! Opens the live database with SQLITE_OPEN_READ_ONLY so it is safe to run
//! while the app holds the WAL.

use rusqlite::{Connection, OpenFlags};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: forge_task_audit <path-to.db>")?;
    let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    let mut stmt = db.prepare(
        "SELECT f.title, t.feature_id, t.id, t.feature_revision, t.state, t.created_at,
                COALESCE(json_extract(t.draft_json,'$.title'),'?')
         FROM work_tasks t
         JOIN forge_features f ON f.id = t.feature_id
         WHERE t.feature_id IS NOT NULL
         ORDER BY f.title, COALESCE(json_extract(t.draft_json,'$.title'),'?'), t.created_at",
    )?;

    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<u64>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    println!("{} feature-owned task(s)\n", rows.len());

    let mut previous: Option<(String, String)> = None;
    let mut run = 0usize;
    for (feature_title, feature_id, id, revision, state, created, task_title) in &rows {
        let key = (feature_id.clone(), task_title.clone());
        if previous.as_ref() == Some(&key) {
            run += 1;
        } else {
            if run > 0 {
                println!();
            }
            run = 0;
            println!("FEATURE {feature_title}  [{feature_id}]");
            previous = Some(key);
        }
        let flag = if run > 0 { "  <== DUPLICATE" } else { "" };
        println!("  {state:<11} rev={revision:?} {created}  {task_title}\n    id={id}{flag}");
    }

    println!("\n--- tables referencing work_tasks ---");
    let mut schema = db.prepare(
        "SELECT name, sql FROM sqlite_master WHERE type='table' AND sql LIKE '%work_tasks%' AND name<>'work_tasks'",
    )?;
    for row in schema.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (name, sql) = row?;
        let cascade = if sql.contains("ON DELETE CASCADE") {
            "CASCADE"
        } else {
            "no cascade"
        };
        println!("  {name:<26} {cascade}");
    }

    println!("\n--- rows attached to each duplicate ---");
    for id in std::env::args().skip(2) {
        let revisions: i64 = db.query_row(
            "SELECT count(*) FROM work_task_revisions WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let as_dependent: i64 = db.query_row(
            "SELECT count(*) FROM work_task_dependencies WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        // RESTRICT, not CASCADE: a row here blocks deleting the prerequisite.
        let as_prerequisite: i64 = db.query_row(
            "SELECT count(*) FROM work_task_dependencies WHERE prerequisite_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let dispatched: i64 = db.query_row(
            "SELECT count(*) FROM pulsation_tasks WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let evidence: i64 = db.query_row(
            "SELECT count(*) FROM task_evidence WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let conversions: i64 = db.query_row(
            "SELECT count(*) FROM forge_task_conversions WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let (state, active) = db.query_row(
            "SELECT state, COALESCE(active_pulsation_id,'-') FROM work_tasks WHERE id=?1",
            [&id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        println!(
            "  {id}  state={state} active_pulsation={active}\n    revisions={revisions} depends_on={as_dependent} blocks={as_prerequisite} dispatched={dispatched} evidence={evidence} conversions={conversions}"
        );
    }

    println!("\n--- summary by state ---");
    let mut summary = db.prepare(
        "SELECT state, count(*) FROM work_tasks WHERE feature_id IS NOT NULL GROUP BY state ORDER BY state",
    )?;
    for row in summary.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (state, count) = row?;
        println!("  {state:<12} {count}");
    }
    Ok(())
}
