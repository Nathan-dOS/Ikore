//! One-off repair for a task the Feature Agent re-proposed with the exact
//! title of an already-`done` task of the same feature (the duplicate-task
//! bug fixed in `feature_forge::service::apply_approval` on 2026-09-18).
//! Delete after use.
//!
//! Everything runs in one transaction and is verified before commit: if any
//! check fails the transaction rolls back and the database is untouched.
//! Pass --commit to actually write; without it the transaction is rolled back
//! after reporting what it would have done.

use rusqlite::{Connection, TransactionBehavior, params};

const FEATURE: &str = "c1433cb6-f4ec-4412-b58c-f29ed283516d";
/// "Define lightweight local browser architecture", created 2026-09-18,
/// duplicating the already-done task below verbatim.
const DUPLICATE: &str = "7ffee358-5726-473a-b1cb-ab2de3d91565";
/// The same task, actually delivered on 2026-09-08.
const DONE: &str = "18fb6745-d959-4030-adb9-420c0305b1fe";
/// Tasks whose `dependencies` (both the `work_task_dependencies` edge and
/// their own `draft_json`) point at the duplicate instead of the done task.
const DEPENDENTS: [&str; 2] = [
    "09c576b9-3ab1-4c0b-b6b1-f3d98b9cd6a4",
    "cef41c63-56c4-4fa0-9611-e3b94b3679a4",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: forge_duplicate_task_repair_2026_09_18 <path-to.db> [--commit]")?;
    let commit = std::env::args().any(|a| a == "--commit");

    let mut db = Connection::open(&path)?;
    db.pragma_update(None, "foreign_keys", true)?;
    let enforced: bool = db.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    assert!(
        enforced,
        "foreign key enforcement is off; refusing to write"
    );

    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;

    // Refuse to touch anything that carries real history.
    let (state, dispatched, evidence, own_deps): (String, i64, i64, i64) = tx.query_row(
        "SELECT w.state,
                (SELECT count(*) FROM pulsation_tasks WHERE task_id=w.id),
                (SELECT count(*) FROM task_evidence WHERE task_id=w.id),
                (SELECT count(*) FROM work_task_dependencies WHERE task_id=w.id)
         FROM work_tasks w WHERE w.id=?1",
        [DUPLICATE],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    assert_eq!(state, "ready", "duplicate is no longer ready; aborting");
    assert_eq!(dispatched, 0, "duplicate has been dispatched; aborting");
    assert_eq!(evidence, 0, "duplicate carries evidence; aborting");
    assert_eq!(own_deps, 0, "duplicate has its own dependencies; aborting");
    let done_state: String =
        tx.query_row("SELECT state FROM work_tasks WHERE id=?1", [DONE], |r| {
            r.get(0)
        })?;
    assert_eq!(done_state, "done", "expected id is not done; aborting");

    // Repoint every dependent's edge and its own draft_json (the id is a
    // globally unique UUID, so a plain substring replace cannot collide with
    // anything else in that JSON).
    for dependent in DEPENDENTS {
        let moved = tx.execute(
            "UPDATE work_task_dependencies SET prerequisite_id=?2 WHERE task_id=?1 AND prerequisite_id=?3",
            params![dependent, DONE, DUPLICATE],
        )?;
        assert_eq!(
            moved, 1,
            "expected exactly one edge to repoint for {dependent}"
        );

        let updated_task = tx.execute(
            "UPDATE work_tasks SET draft_json=REPLACE(draft_json,?2,?3) WHERE id=?1 AND draft_json LIKE '%'||?2||'%'",
            params![dependent, DUPLICATE, DONE],
        )?;
        assert_eq!(
            updated_task, 1,
            "expected exactly one draft_json rewrite in work_tasks for {dependent}"
        );

        let updated_revision = tx.execute(
            "UPDATE work_task_revisions SET draft_json=REPLACE(draft_json,?2,?3) WHERE task_id=?1 AND draft_json LIKE '%'||?2||'%'",
            params![dependent, DUPLICATE, DONE],
        )?;
        assert_eq!(
            updated_revision, 1,
            "expected exactly one draft_json rewrite in work_task_revisions for {dependent}"
        );

        println!("repointed {dependent} -> {DONE}");
    }

    let removed = tx.execute("DELETE FROM work_tasks WHERE id=?1", [DUPLICATE])?;
    assert_eq!(
        removed, 1,
        "expected to delete exactly one row for the duplicate"
    );
    println!("deleted {DUPLICATE}");

    // Post-state: exactly one task with this title, done, under its original id.
    let matching: Vec<(String, String)> = tx
        .prepare("SELECT id,state FROM work_tasks WHERE feature_id=?1 AND json_extract(draft_json,'$.title')='Define lightweight local browser architecture'")?
        .query_map([FEATURE], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        matching,
        vec![(DONE.to_string(), "done".to_string())],
        "unexpected post-state for the architecture task; rolling back"
    );
    for dependent in DEPENDENTS {
        let deps: Vec<String> = tx
            .prepare("SELECT prerequisite_id FROM work_task_dependencies WHERE task_id=?1 ORDER BY prerequisite_id")?
            .query_map([dependent], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        assert!(
            deps.contains(&DONE.to_string()) && !deps.contains(&DUPLICATE.to_string()),
            "{dependent} dependency edges still wrong: {deps:?}"
        );
        let draft_json: String = tx.query_row(
            "SELECT draft_json FROM work_tasks WHERE id=?1",
            [dependent],
            |r| r.get(0),
        )?;
        assert!(
            !draft_json.contains(DUPLICATE),
            "{dependent} draft_json still references the duplicate"
        );
    }

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
