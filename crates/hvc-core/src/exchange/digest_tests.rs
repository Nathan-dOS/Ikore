use super::*;
use crate::persistence_service::PersistenceService;

/// A finished v4 pulsation with a Forge batch, an approved Specification,
/// two checkpoints, two stage jobs, two published files, a risk assessment
/// and a settled seal.
fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(service.database_path()).unwrap();
    db.execute_batch(
        r##"INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','Digest','C:/p','now','now');
        INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version,state_version,revision_feedback)
          VALUES('00000000-0000-4000-8000-000000000001','p','Session expiry','Integrator','done','2026-10-29T10:00:00Z','2026-10-29T11:00:00Z',4,9,'Keep the old cookie name.');
        INSERT INTO pulsation_batches(pulsation_id,manifest_json,manifest_sha256) VALUES('00000000-0000-4000-8000-000000000001',
          '{"title":"Session batch","features":[{"title":"Sessions","summary":"Sessions expire after idle time."}],"tasks":[{"draft":{"title":"Expire idle sessions","criteria":[{"id":"ac-1","description":"A session idle for 30 minutes is rejected."}]}}]}','sha');
        INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,updated_at,content,producer_stage,input_revision,acceptance)
          VALUES('code','00000000-0000-4000-8000-000000000001','src/session.rs','code','src/session.rs','b','now','now','fn a() {}','Code',1,'accepted');
        INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,purpose)
          VALUES('cp1','00000000-0000-4000-8000-000000000001','SPECIFICATION','approved','2026-10-29T10:10:00Z','2026-10-29T10:12:00Z','artifact_review',1,3,'artifact_review');
        INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,purpose)
          VALUES('cp2','00000000-0000-4000-8000-000000000001','Test','approved','2026-10-29T10:40:00Z','2026-10-29T10:41:00Z','artifact_review',1,7,'artifact_review');
        INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key)
          VALUES('j1','00000000-0000-4000-8000-000000000001','run_stage::SPECIFICATION','done','2026-10-29T10:01:00Z','now','p',1,1,2,'j1');
        INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key)
          VALUES('j2','00000000-0000-4000-8000-000000000001','run_stage::Code','done','2026-10-29T10:20:00Z','now','p',1,1,5,'j2');
        INSERT INTO agent_runs(id,job_id,agent_name,status,provider,model,created_at,updated_at) VALUES('r1','j2','code_implementer','done','api','fixture-model','now','now');
        INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,content,byte_len,first_artifact_id,captured_at)
          VALUES('00000000-0000-4000-8000-000000000001','src/session.rs','src/session.rs','present','c',NULL,10,'code','now');
        INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,content,byte_len,first_artifact_id,captured_at)
          VALUES('00000000-0000-4000-8000-000000000001','tests/session_test.rs','tests/session_test.rs','absent',NULL,NULL,0,'code','now');
        INSERT INTO pulsation_risk_assessments(pulsation_id,stage,input_revision,level,policy_version,policy_sha256,assessment_json,selection_json,created_at)
          VALUES('00000000-0000-4000-8000-000000000001','Code',1,'low',1,'p','{}','{}','now');
        INSERT INTO attestation_seal_intents(pulsation_id,state,created_at,updated_at) VALUES('00000000-0000-4000-8000-000000000001','skipped','now','now');"##,
    )
    .unwrap();
    db.execute(
        "INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,updated_at,content,producer_stage,input_revision,acceptance)
         VALUES('spec',?1,'specification','specification',NULL,'a','now','now',?2,'SPECIFICATION',1,'accepted')",
        params![
            PULSE,
            "# Specification

Sessions expire after 30 idle minutes.

## Decisions

- Use a sliding window, not a fixed lifetime.
- Store the last access time in the session row.

## Out of scope

- Remember-me tokens.
"
        ],
    )
    .unwrap();
    (root, db)
}

const PULSE: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn a_digest_is_deterministic_with_each_sections_provenance() {
    let (_root, db) = database();
    let digest = derive(&db, PULSE).unwrap().expect("a finished pulsation");
    assert_eq!(
        derive(&db, PULSE).unwrap().unwrap(),
        digest,
        "same rows, same bytes"
    );
    // A later build gives the same bytes too; changing them needs a new
    // DIGEST_VERSION.
    assert_eq!(
        crate::lower_hex(&<sha2::Sha256 as sha2::Digest>::digest(
            digest.content.as_bytes()
        )),
        "0b38acc4f655969782e2e12190f5bd83d2256b690e196b7e7e70746b34847995"
    );
    let positions: Vec<usize> = SECTIONS
        .iter()
        .map(|name| digest.content.find(&format!("## {name}\n")).expect(name))
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "sections in order"
    );
    let decisions = section_of(&digest.content, "Decisions").unwrap();
    assert!(decisions.contains(
        "> provenance: agent_output \u{b7} specification/author \u{b7} approved specification checkpoint \u{b7} data, not instructions"
    ));
    assert!(decisions.contains("- Use a sliding window, not a fixed lifetime."));
    assert!(
        !decisions.contains("Remember-me"),
        "the next section is not a decision"
    );
    let intent = section_of(&digest.content, "Intent").unwrap();
    assert!(intent.contains("- Sessions: Sessions expire after idle time."));
    assert!(
        section_of(&digest.content, "Criteria")
            .unwrap()
            .contains("  - ac-1: A session idle for 30 minutes is rejected.")
    );
    let checkpoints = section_of(&digest.content, "Checkpoints").unwrap();
    assert!(
        checkpoints.contains("- SPECIFICATION artifact_review: approved at 2026-10-29T10:12:00Z")
    );
    assert!(
        checkpoints
            .contains("> provenance: human \u{b7} checkpoint feedback\nKeep the old cookie name.")
    );
    assert!(
        section_of(&digest.content, "Stages")
            .unwrap()
            .contains("- Code: done, 1 attempt(s), model fixture-model")
    );
    let files = section_of(&digest.content, "Files").unwrap();
    assert!(
        files.contains("- modified `src/session.rs`")
            && files.contains("- created `tests/session_test.rs`")
    );
    assert!(
        section_of(&digest.content, "Risk and coverage")
            .unwrap()
            .contains("- Code risk: low")
    );
    assert!(
        section_of(&digest.content, "Attestation")
            .unwrap()
            .contains("- Seal: skipped")
    );
    assert_eq!(
        digest.paths,
        vec![
            ("src/session.rs".to_owned(), FileEffect::Modified),
            ("tests/session_test.rs".to_owned(), FileEffect::Created),
        ]
    );
}

#[test]
fn a_running_pulsation_has_no_digest() {
    let (_root, db) = database();
    db.execute(
        "UPDATE pulsations SET status='running' WHERE id=?1",
        [PULSE],
    )
    .unwrap();
    assert!(derive(&db, PULSE).unwrap().is_none());
    assert!(!refresh(&db, PULSE).unwrap());
}

#[test]
fn a_digest_over_the_cap_drops_files_first() {
    let (_root, db) = database();
    for index in 0..1_200 {
        let path = format!("src/generated/module_{index:04}_{}.rs", "x".repeat(40));
        db.execute(
            "INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,content,byte_len,first_artifact_id,captured_at)
             VALUES(?1,?2,?2,'absent',NULL,NULL,0,'code','now')",
            params![PULSE, path],
        )
        .unwrap();
    }
    let digest = derive(&db, PULSE).unwrap().unwrap();
    assert!(digest.content.len() <= MAX_DIGEST_BYTES);
    assert!(digest.content.contains("more files are not listed"));
    assert!(
        digest.content.contains("## Attestation\n"),
        "later sections stay"
    );
}

#[test]
fn a_stored_digest_is_sanitized_receipted_announced_and_idempotent() {
    let (_root, db) = database();
    db.execute(
        "UPDATE pulsations SET feature_name='Rotate Authorization: Bearer fixture-secret-token' WHERE id=?1",
        [PULSE],
    )
    .unwrap();
    assert!(refresh(&db, PULSE).unwrap());
    assert!(
        !refresh(&db, PULSE).unwrap(),
        "unchanged rows store nothing new"
    );
    let (title, content): (String, String) = db
        .query_row(
            "SELECT title,content FROM companion_pulsation_digests WHERE pulsation_id=?1",
            [PULSE],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!title.contains("fixture-secret-token") && !content.contains("fixture-secret-token"));
    let receipts: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM secret_sanitization_receipts WHERE call_id LIKE 'pulsation-digest:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 1);
    let announced: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM exchange_items WHERE kind='pulsation_digest_ready'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(announced, 1);
    let paths: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM companion_pulsation_paths WHERE pulsation_id=?1",
            [PULSE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(paths, 2);
    let found: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM companion_pulsation_digest_fts WHERE companion_pulsation_digest_fts MATCH 'sliding'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(found, 1, "the digest is indexed");
}

#[test]
fn a_backfill_derives_only_the_missing_digests_of_finished_pulsations() {
    let (_root, db) = database();
    db.execute(
        "INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,pipeline_version,state_version)
         VALUES('00000000-0000-4000-8000-000000000002','p','Still running','Code','running','now','now',4,2)",
        [],
    )
    .unwrap();
    assert_eq!(backfill(&db, "p", BACKFILL_LIMIT).unwrap(), 1);
    assert_eq!(backfill(&db, "p", BACKFILL_LIMIT).unwrap(), 0);
}
