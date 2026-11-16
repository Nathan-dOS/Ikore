//! End-to-end pipeline v4 (ADR-022, single unit per stage) over the real
//! supervisor, router and SQLite with a scripted loopback provider.
//!
//! Every stage runs exactly one worker whose reply is a valid
//! `UnitModelResponseV1` built from the handles the host rendered; nothing
//! in the reply is invented. Human checkpoints stay where the legacy pipeline
//! has them and the accepted stage is provenanced with its execution.
mod composition_support;

use composition_support::*;
use std::fs;

#[test]
fn composed_batch_runs_six_stages_with_one_invocation_each() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    let db = harness.db();
    let version: u8 = db
        .query_row(
            "SELECT pipeline_version FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, 4, "a batch of a composition-enabled project is v4");

    for stage in ["SPECIFICATION", "Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        let decisions = harness
            .service
            .list_pending_decisions(&harness.project)
            .unwrap();
        assert!(
            decisions
                .iter()
                .any(|decision| decision.pulsation_id.as_deref() == Some(harness.pulse.as_str())),
            "the dock projects the current checkpoint without creating chat state"
        );
        for table in [
            "companion_dock_sessions",
            "companion_drafts",
            "decision_conversations",
        ] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                count, 0,
                "ordinary pipeline transitions and decision reads must not write {table}"
            );
        }
        harness.approve();
    }
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT status='done' FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
    });
    for table in [
        "companion_dock_sessions",
        "companion_drafts",
        "decision_conversations",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "a completed six-stage pulse must not create {table}"
        );
    }

    // Exactly one real invocation per stage, in stage order, and no
    // coordination call: the promotion gate for arm B (single unit).
    assert_eq!(
        harness.calls(),
        [
            "intake_classifier",
            "specification_author",
            "contract_author",
            "code_implementer",
            "test_author",
            "integration_reviewer"
        ]
    );

    let pulse = &harness.pulse;
    let accepted: Vec<(String, String, Option<String>)> = db
        .prepare("SELECT e.stage,e.status,a.stage_execution_id FROM stage_executions e LEFT JOIN stage_acceptances a ON a.stage_execution_id=e.id WHERE e.pulsation_id=?1 ORDER BY e.created_at")
        .unwrap()
        .query_map([pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(accepted.len(), 6, "{accepted:?}");
    for (stage, status, provenance) in &accepted {
        assert_eq!(status, "accepted", "{stage}");
        assert!(
            provenance.is_some(),
            "{stage} acceptance carries its execution"
        );
    }
    let attempts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND a.state='succeeded'",
            [pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 6);
    let reservations: (i64, i64) = db
        .query_row(
            "SELECT SUM(pool='initial'),SUM(state='settled') FROM stage_invocation_reservations r JOIN stage_executions e ON e.id=r.execution_id WHERE e.pulsation_id=?1",
            [pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        reservations,
        (6, 6),
        "every call held an initial permit that settled"
    );
    let attributed: i64 = db
        .query_row(
            "SELECT COUNT(DISTINCT attempt_id) FROM token_usage_records WHERE stage_execution_id IS NOT NULL AND pulsation_id=?1",
            [pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attributed, 6, "usage is attributed to every attempt");
    let evidence: Vec<(String, String)> = db
        .prepare("SELECT stage,evidence_json FROM task_evidence WHERE pulsation_id=?1 AND task_id=?2 ORDER BY stage")
        .unwrap()
        .query_map([pulse, &harness.task_id().to_owned()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(evidence.len(), 6, "{evidence:?}");
    assert!(evidence.iter().all(|(_, ids)| ids.contains("ac-1")));

    // The host reconstructed the files the workers edited and published them
    // through the outbox with the hashes observed at admission.
    let workspace = harness.env.workspace_dir();
    assert_eq!(
        fs::read_to_string(workspace.join("src/auth.rs")).unwrap(),
        IMPLEMENTED_SOURCE
    );
    assert_eq!(
        fs::read_to_string(workspace.join("tests/auth_test.rs")).unwrap(),
        AUTHORED_TEST
    );
    let notes = fs::read_dir(workspace.join("knowledge/obsidian/features"))
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("reference-contract: stage-composition-v1"));
    assert!(notes[0].contains("An expired token is rejected by validate_token"));
    let payloads: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM artifacts WHERE pulsation_id=?1 AND kind='stage_payload_v1' AND acceptance='accepted'",
            [pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(payloads, 6, "every stage persisted its typed payload");
}
