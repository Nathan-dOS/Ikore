use std::fs;

use rusqlite::TransactionBehavior;

use super::super::serial::tests::Fixture;
use super::*;
use crate::agent_runtime::GeneratedArtifact;
use crate::orchestration::ContentSha256;
use crate::persistence_service::{AgentRuntimeConfig, PulsationStage, PulsationStatus};

fn artifact(path: &str, content: &str) -> GeneratedArtifact {
    GeneratedArtifact {
        name: path.into(),
        kind: "stage_output".into(),
        relative_path: Some(path.into()),
        content: content.into(),
    }
}

/// Reticle P1 with PD6: the publisher records what each publication
/// replaced. A new file has nothing before it; a revision round's delivery
/// replaced the previous one, whose text outlives the pulsation's base.
#[test]
fn the_publisher_records_what_each_publication_replaced() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output_artifacts(
        "advance",
        vec![
            artifact("src/code.rs", "first\n"),
            artifact("src/fresh.rs", "fresh\n"),
        ],
    ))
    .unwrap();
    let project = f.pulse.project_id.clone();
    let pending = f.service.job_changes(&project, &f.job.id).unwrap();
    assert_eq!(pending.len(), 2);
    assert!(
        pending
            .iter()
            .all(|change| change.before == PublishedBefore::NotApplied)
    );
    f.service.reconcile_publications().unwrap();
    let first = f.service.job_changes(&project, &f.job.id).unwrap();
    let paths: Vec<_> = first
        .iter()
        .map(|change| change.relative_path.as_str())
        .collect();
    assert_eq!(paths, ["src/code.rs", "src/fresh.rs"]);
    assert!(
        first
            .iter()
            .all(|change| change.publication_state == "applied"
                && change.before == PublishedBefore::Absent)
    );
    assert_eq!(first[0].after, "first\n");
    assert!(
        f.service
            .job_changes("another-project", &f.job.id)
            .unwrap()
            .is_empty()
    );

    // A revision round of Code publishes over its first delivery.
    let db = f.service.connection().unwrap();
    db.execute("UPDATE artifacts SET acceptance='superseded'", [])
        .unwrap();
    db.execute(
        "UPDATE pulsations SET stage='Code',status='planning' WHERE id=?1",
        [&f.pulse.id],
    )
    .unwrap();
    db.execute(
        "UPDATE jobs SET kind='run_stage::Code' WHERE status='pending'",
        [],
    )
    .unwrap();
    let (job, _) = f.service.claim_next_pending_job().unwrap().unwrap();
    let tx = db.unchecked_transaction().unwrap();
    let current = PersistenceService::find_pulsation_by_id(&tx, &f.pulse.id)
        .unwrap()
        .unwrap();
    PersistenceService::transition_pulsation(
        &tx,
        &current,
        PulsationStatus::Running,
        PulsationStage::Code,
        "test_revision",
        None,
    )
    .unwrap();
    tx.commit().unwrap();
    let run = f
        .service
        .start_agent_run_for_job(&job, &AgentRuntimeConfig::default())
        .unwrap();
    let fence = PersistenceService::execution_fence(&db, &job.id).unwrap();
    let handoff = f
        .service
        .load_stage_handoff(&fence, PulsationStage::Code, 16000)
        .unwrap();
    f.service
        .accept_stage_result(
            &job,
            &run,
            &handoff,
            &f.output("advance", Some("src/code.rs"), "second\n"),
            f.workspace.path(),
        )
        .unwrap();
    // The publisher needs the job's runtime slot back, as `Fixture::accept` does.
    f.service
        .release_runtime_slot(&PersistenceService::job_slot_token(&job))
        .unwrap();
    f.service.unregister_job_cancellation(&job.id);
    f.service.reconcile_publications().unwrap();
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("src/code.rs")).unwrap(),
        "second\n"
    );
    let second = f.service.job_changes(&project, &job.id).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].before, PublishedBefore::Text("first\n".into()));
    assert_eq!(second[0].after, "second\n");
    let one = f
        .service
        .published_change(&project, &second[0].artifact_id)
        .unwrap();
    assert_eq!(one.as_ref(), Some(&second[0]));
    // ADR-032 clears the pulsation's bases when it ends; the preimage stays.
    db.execute("UPDATE pulsation_file_bases SET content=NULL", [])
        .unwrap();
    assert_eq!(
        f.service.job_changes(&project, &job.id).unwrap()[0].before,
        PublishedBefore::Text("first\n".into())
    );
}
/// A text is stored once per project; over the budget, texts no publication
/// refers to go first, and a text that still does not fit is refused.
#[test]
fn the_budget_drops_unreferenced_texts_before_refusing_one() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "new\n"))
        .unwrap();
    let project = f.pulse.project_id.clone();
    let mut db = f.service.connection().unwrap();
    let artifact_id: String = db
        .query_row("SELECT artifact_id FROM artifact_publications", [], |row| {
            row.get(0)
        })
        .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let sha = |text: &str| ContentSha256::of(text.as_bytes()).as_str().to_owned();
    // `old\n` is what the publication replaced: a text it refers to.
    let capture = PublicationBase {
        state: "present",
        sha256: Some(sha("old\n")),
        content: Some("old\n".into()),
        byte_len: Some(4),
        reason: None,
    };
    let replaced = Replaced {
        project_id: &project,
        pulsation_id: &f.pulse.id,
        artifact_id: &artifact_id,
        relative_path: "code.rs",
        capture: &capture,
    };
    record_within(&tx, &replaced, "now", 1024).unwrap();
    let blobs = |tx: &Transaction<'_>| -> Vec<String> {
        tx.prepare(
            "SELECT content FROM publication_preimage_blobs WHERE project_id=?1 ORDER BY content",
        )
        .unwrap()
        .query_map([&project], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(blobs(&tx), ["old\n"]);
    // Already stored: nothing added, whatever the budget.
    assert!(store_within(&tx, &project, &sha("old\n"), "old\n", "now", 0).unwrap());
    // Room for one more.
    assert!(store_within(&tx, &project, &sha("spare"), "spare", "now", 9).unwrap());
    assert_eq!(blobs(&tx), ["old\n", "spare"]);
    // Over the budget, the unreferenced `spare` goes first.
    assert!(store_within(&tx, &project, &sha("other"), "other", "now", 9).unwrap());
    assert_eq!(blobs(&tx), ["old\n", "other"]);
    // A text that does not fit beside the referenced one is refused.
    assert!(!store_within(&tx, &project, &sha("wide text"), "wide text", "now", 9).unwrap());
    assert_eq!(blobs(&tx), ["old\n"]);
}

/// A replaced text that does not fit is recorded by hash, with the reason,
/// and the change says it is unavailable instead of showing nothing.
#[test]
fn a_text_that_does_not_fit_is_recorded_by_hash_with_the_reason() {
    let f = Fixture::new(PulsationStage::Code);
    f.accept(&f.output("advance", Some("code.rs"), "new\n"))
        .unwrap();
    let project = f.pulse.project_id.clone();
    let mut db = f.service.connection().unwrap();
    let artifact_id: String = db
        .query_row("SELECT artifact_id FROM artifact_publications", [], |row| {
            row.get(0)
        })
        .unwrap();
    let old = ContentSha256::of(b"old\n").as_str().to_owned();
    let capture = PublicationBase {
        state: "present",
        sha256: Some(old.clone()),
        content: Some("old\n".into()),
        byte_len: Some(4),
        reason: None,
    };
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let replaced = Replaced {
        project_id: &project,
        pulsation_id: &f.pulse.id,
        artifact_id: &artifact_id,
        relative_path: "code.rs",
        capture: &capture,
    };
    record_within(&tx, &replaced, "now", 0).unwrap();
    // A replay keeps the first record.
    record_within(&tx, &replaced, "later", 1024).unwrap();
    tx.commit().unwrap();
    let (state, sha256, reason): (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT state,content_sha256,reason FROM publication_preimages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "present_unstored");
    assert_eq!(sha256.as_deref(), Some(old.as_str()));
    assert_eq!(reason.as_deref(), Some("over_budget"));
    let changes = f.service.job_changes(&project, &f.job.id).unwrap();
    assert_eq!(
        changes[0].before,
        PublishedBefore::Unavailable("over_budget".into())
    );
}
