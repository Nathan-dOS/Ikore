//! One-off repair for the tasks duplicated by the repeated-approval regression
//! (see 21-Forge-Approval-Idempotence-and-Forge-Columns-2026-09-10). Delete
//! after use.
//!
//! Everything runs in one transaction and is verified before commit: if any
//! check fails the transaction rolls back and the database is untouched.
//! Pass --commit to actually write; without it the transaction is rolled back
//! after reporting what it would have done.

use rusqlite::{Connection, TransactionBehavior, params};

/// Deleted in reverse dependency order: `work_task_dependencies.prerequisite_id`
/// is ON DELETE RESTRICT, so a task that is still someone's prerequisite cannot
/// go first. Each delete cascades away the edges it owns, freeing the next.
const DUPLICATES: [&str; 3] = [
    "bd3930d0-e9eb-4d3b-9c3b-703bed03884f", // blocks nothing
    "8c4c50b1-c2e9-4748-b804-542f6debdf19", // blocked only by the one above
    "2947abc4-94ec-41bf-98a2-55c38b85d20a", // prerequisite of both above
];

/// Were `ready` before the bad approval superseded them.
const RESTORE: [&str; 2] = [
    "d09f8094-955e-4506-91e8-09b7fbd2061e",
    "a14805a4-3e22-461d-bccc-e44fd2bec791",
];

const FEATURE: &str = "c1433cb6-f4ec-4412-b58c-f29ed283516d";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: forge_task_repair <path-to.db> [--commit]")?;
    let commit = std::env::args().any(|a| a == "--commit");

    let mut db = Connection::open(&path)?;
    // Without this the CASCADE rules do not run and the RESTRICT rules do not
    // protect: the delete would silently orphan revisions and dependency edges.
    db.pragma_update(None, "foreign_keys", true)?;
    let enforced: bool = db.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    assert!(
        enforced,
        "foreign key enforcement is off; refusing to write"
    );

    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Refuse to touch anything that carries real history.
    for id in DUPLICATES {
        let (state, dispatched, evidence): (String, i64, i64) = tx.query_row(
            "SELECT w.state,
                    (SELECT count(*) FROM pulsation_tasks WHERE task_id=w.id),
                    (SELECT count(*) FROM task_evidence WHERE task_id=w.id)
             FROM work_tasks w WHERE w.id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!(state, "ready", "{id} is no longer ready; aborting");
        assert_eq!(dispatched, 0, "{id} has been dispatched; aborting");
        assert_eq!(evidence, 0, "{id} carries evidence; aborting");
    }
    for id in RESTORE {
        let state: String =
            tx.query_row("SELECT state FROM work_tasks WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        assert_eq!(state, "superseded", "{id} is not superseded; aborting");
    }

    for id in DUPLICATES {
        let removed = tx.execute("DELETE FROM work_tasks WHERE id=?1", [id])?;
        assert_eq!(removed, 1, "expected to delete exactly one row for {id}");
        println!("deleted {id}");
    }
    for id in RESTORE {
        let restored = tx.execute(
            "UPDATE work_tasks SET state='ready',updated_at=?2 WHERE id=?1 AND state='superseded'",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        assert_eq!(restored, 1, "expected to restore exactly one row for {id}");
        println!("restored {id} -> ready");
    }

    // Post-state must be exactly the three originals: one done, two ready.
    let mut stmt = tx.prepare(
        "SELECT state, count(*) FROM work_tasks WHERE feature_id=?1 GROUP BY state ORDER BY state",
    )?;
    let summary = stmt
        .query_map([FEATURE], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    println!("post-state: {summary:?}");
    assert_eq!(
        summary,
        vec![("done".to_string(), 1), ("ready".to_string(), 2)],
        "unexpected post-state; rolling back"
    );

    let orphans: i64 = tx.query_row(
        "SELECT (SELECT count(*) FROM work_task_revisions r WHERE NOT EXISTS(SELECT 1 FROM work_tasks w WHERE w.id=r.task_id))
              + (SELECT count(*) FROM work_task_dependencies d WHERE NOT EXISTS(SELECT 1 FROM work_tasks w WHERE w.id=d.task_id)
                                                                  OR NOT EXISTS(SELECT 1 FROM work_tasks w WHERE w.id=d.prerequisite_id))",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(orphans, 0, "left orphaned rows behind; rolling back");

    if commit {
        tx.commit()?;
        println!("\nCOMMITTED");
    } else {
        println!("\nDRY RUN - rolled back, database unchanged (pass --commit to apply)");
    }
    Ok(())
}
