use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::llm::{CredentialStore, LlmError, LlmErrorKind};
use crate::persistence_service::serial::tests::Fixture;
use crate::persistence_service::{CreatePulsationRequest, PulsationStatus};

/// The operating system's credential store, in memory. `locked` makes every
/// call fail the way an unavailable keyring does.
#[derive(Default)]
struct Keys {
    secrets: Mutex<HashMap<String, String>>,
    locked: AtomicBool,
}

impl Keys {
    fn check(&self) -> Result<(), LlmError> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(LlmError::new(LlmErrorKind::Persistence, "locked"));
        }
        Ok(())
    }

    fn secret(&self) -> Option<String> {
        self.secrets
            .lock()
            .unwrap()
            .get(KEY_CREDENTIAL_REF)
            .cloned()
    }
}

impl CredentialStore for Keys {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError> {
        self.check()?;
        Ok(self.secrets.lock().unwrap().get(credential_ref).cloned())
    }

    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        self.check()?;
        self.secrets
            .lock()
            .unwrap()
            .insert(credential_ref.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError> {
        self.check()?;
        Ok(self
            .secrets
            .lock()
            .unwrap()
            .remove(credential_ref)
            .is_some())
    }
}

fn fixture() -> (Fixture, Arc<Keys>) {
    let mut f = Fixture::new(PulsationStage::Code);
    let keys = Arc::new(Keys::default());
    f.service.credentials = keys.clone();
    (f, keys)
}

fn sha(content: &str) -> String {
    ContentSha256::of(content.as_bytes()).as_str().to_owned()
}

/// Writes `content` at `path` in the workspace and records it as a main
/// publication of the fixture's pulsation in `state`.
fn publish(f: &Fixture, id: &str, path: &str, content: &str, state: &str) {
    let file = f.workspace.path().join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, content).unwrap();
    let db = f.service.connection().unwrap();
    db.execute(
        "INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,content,created_at,updated_at,producer_stage,acceptance,publication_state)
         VALUES(?1,?2,?3,'source',?3,?4,?5,'2026-10-26T12:00:00Z','2026-10-26T12:00:00Z','Code','accepted',?6)",
        params![id, f.pulse.id, path, sha(content), content, state],
    )
    .unwrap();
    db.execute(
        "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,desired_sha256,state,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'2026-10-26T12:00:00Z')",
        params![id, f.pulse.project_id, f.pulse.id, path, sha(content), state],
    )
    .unwrap();
}

fn end_pulsation(
    service: &PersistenceService,
    pulsation_id: &str,
    status: PulsationStatus,
    reason: Option<&str>,
) {
    let db = service.connection().unwrap();
    let tx = db.unchecked_transaction().unwrap();
    if let Some(reason) = reason {
        tx.execute(
            "UPDATE pulsations SET terminal_reason=?2 WHERE id=?1",
            params![pulsation_id, reason],
        )
        .unwrap();
    }
    let current = PersistenceService::find_pulsation_by_id(&tx, pulsation_id)
        .unwrap()
        .unwrap();
    PersistenceService::transition_pulsation(
        &tx,
        &current,
        status,
        current.stage,
        "test_end",
        None,
    )
    .unwrap();
    tx.commit().unwrap();
}

fn end(f: &Fixture, status: PulsationStatus, reason: Option<&str>) {
    end_pulsation(&f.service, &f.pulse.id, status, reason);
}

/// A second pulsation of the fixture's project, failed before it started.
fn failed_sibling(f: &Fixture) -> String {
    let pulse = f
        .service
        .create_pulsation(CreatePulsationRequest {
            project_id: f.pulse.project_id.clone(),
            feature_name: "A sibling".into(),
        })
        .unwrap()
        .pulsation;
    end_pulsation(&f.service, &pulse.id, PulsationStatus::Failed, None);
    pulse.id
}

fn seal_all(service: &PersistenceService) {
    while service.seal_next_attestation().unwrap() {}
}

fn status(f: &Fixture) -> AttestationStatus {
    f.service.attestation_status(&f.pulse.id).unwrap()
}

fn verify(f: &Fixture) -> AttestationVerification {
    f.service.verify_attestation(&f.pulse.id).unwrap().unwrap()
}

fn stored_statement(f: &Fixture) -> String {
    f.service
        .connection()
        .unwrap()
        .query_row(
            "SELECT statement_json FROM pulsation_attestations WHERE pulsation_id=?1",
            [&f.pulse.id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_finished_pulsation_is_sealed_once_and_signed_with_a_first_key() {
    let (f, keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "applied");
    assert_eq!(status(&f).state, AttestationState::NotSealed);
    assert!(!f.service.seal_next_attestation().unwrap());

    end(&f, PulsationStatus::Done, None);
    assert_eq!(status(&f).state, AttestationState::Pending);
    assert!(f.service.seal_next_attestation().unwrap());
    assert!(
        !f.service.seal_next_attestation().unwrap(),
        "a pulsation is sealed once"
    );

    let sealed = status(&f);
    assert_eq!(sealed.state, AttestationState::Signed);
    assert_eq!(sealed.subject_count, 1);
    assert_eq!(sealed.outcome.as_deref(), Some("done"));
    let key = f.service.attestation_key_status().unwrap();
    assert_eq!(key.state, AttestationKeyState::Ready);
    assert!(key.sealing_enabled);
    assert_eq!(key.keys.len(), 1);
    assert_eq!(key.keys[0].state, "active");
    assert_eq!(sealed.key_id.as_ref(), Some(&key.keys[0].key_id));
    assert!(keys.secret().is_some(), "the first seal stored a key");

    let check = verify(&f);
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(check.rederivation, RederivationCheck::Matches);
    assert_eq!(check.difference, None);
    assert_eq!(
        check.workspace,
        vec![SubjectCheck {
            path: "src/lib.rs".into(),
            state: WorkspaceCheck::Unchanged,
        }]
    );
    assert_eq!(
        status(&f).verification,
        Some(check),
        "the last check is kept for display"
    );
    let statement: StatementV1 = serde_json::from_str(&stored_statement(&f)).unwrap();
    assert_eq!(statement.subject[0].digest.sha256, sha("pub fn one() {}\n"));
    assert_eq!(statement.predicate.effects[0].artifact_id, "a1");
    assert_eq!(statement.predicate.builder.key_id, sealed.key_id);
    assert!(statement.predicate.pulsation.ended_at.is_some());
}

#[test]
fn the_outcome_follows_how_the_pulsation_ended() {
    for (status_after, reason, outcome) in [
        (PulsationStatus::Failed, None, "failed"),
        (PulsationStatus::Failed, Some("hitl_rejected"), "rejected"),
        (PulsationStatus::Failed, Some("user_cancel"), "aborted"),
        (
            PulsationStatus::Failed,
            Some("model_reported_failure"),
            "failed",
        ),
    ] {
        let (f, _keys) = fixture();
        publish(&f, "a1", "src/lib.rs", "partial\n", "applied");
        end(&f, status_after, reason);
        seal_all(&f.service);
        let sealed = status(&f);
        assert_eq!(sealed.state, AttestationState::Signed, "{reason:?}");
        assert_eq!(sealed.outcome.as_deref(), Some(outcome), "{reason:?}");
        assert_eq!(sealed.subject_count, 1, "a failure keeps its effects");
    }
}

#[test]
fn sealing_waits_for_the_pulsations_main_publications() {
    let (f, _keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "pending");
    end(&f, PulsationStatus::Done, None);
    assert!(!f.service.seal_next_attestation().unwrap());
    assert_eq!(status(&f).state, AttestationState::Pending);
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE artifact_publications SET state='applying' WHERE artifact_id='a1'",
            [],
        )
        .unwrap();
    assert!(!f.service.seal_next_attestation().unwrap());
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE artifact_publications SET state='applied' WHERE artifact_id='a1'",
            [],
        )
        .unwrap();
    assert!(f.service.seal_next_attestation().unwrap());
    assert_eq!(status(&f).subject_count, 1);
}

#[test]
fn only_the_last_applied_publication_of_a_path_is_a_subject() {
    let (f, _keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "first\n", "applied");
    publish(&f, "a2", "src/old.rs", "conflict\n", "conflict");
    publish(&f, "a3", "SRC/Lib.rs", "second\n", "applied");
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE artifact_publications SET updated_at='2026-10-26T12:00:01Z' WHERE artifact_id='a3'",
            [],
        )
        .unwrap();
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let statement: StatementV1 = serde_json::from_str(&stored_statement(&f)).unwrap();
    assert_eq!(statement.subject.len(), 1);
    assert_eq!(statement.subject[0].name, "SRC/Lib.rs");
    assert_eq!(statement.subject[0].digest.sha256, sha("second\n"));
}

#[test]
fn sealing_can_be_turned_off_for_pulsations_that_end_later() {
    let (f, _keys) = fixture();
    assert!(f.service.attestation_sealing_enabled().unwrap());
    f.service.set_attestation_sealing_enabled(false).unwrap();
    end(&f, PulsationStatus::Done, None);
    assert_eq!(status(&f).state, AttestationState::Skipped);
    assert!(!f.service.seal_next_attestation().unwrap());
    f.service.set_attestation_sealing_enabled(true).unwrap();
    assert!(!f.service.seal_next_attestation().unwrap());
    assert_eq!(
        status(&f).state,
        AttestationState::Skipped,
        "turning sealing back on seals nothing retroactively"
    );
    assert!(
        !f.service
            .attestation_key_status()
            .unwrap()
            .keys
            .iter()
            .any(|_| true)
    );
}

#[test]
fn without_a_keyring_the_statement_is_kept_unsigned_and_signed_later() {
    let (f, keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "applied");
    keys.locked.store(true, Ordering::SeqCst);
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let unsigned = status(&f);
    assert_eq!(unsigned.state, AttestationState::Unsigned);
    assert_eq!(unsigned.key_id, None);
    assert_eq!(
        f.service.attestation_key_status().unwrap().state,
        AttestationKeyState::KeyringUnavailable
    );
    let check = verify(&f);
    assert_eq!(check.signature, SignatureCheck::Unsigned);
    assert_eq!(check.rederivation, RederivationCheck::Matches);
    assert!(matches!(
        f.service.sign_attestation(&f.pulse.id),
        Err(PersistenceServiceError::Validation(_))
    ));
    assert!(matches!(
        f.service.export_attestation(&f.pulse.id),
        Err(PersistenceServiceError::Validation(_))
    ));

    keys.locked.store(false, Ordering::SeqCst);
    let signed = f.service.sign_attestation(&f.pulse.id).unwrap();
    assert_eq!(signed.state, AttestationState::Signed);
    assert_eq!(
        signed.statement_sha256, unsigned.statement_sha256,
        "signing later keeps the statement"
    );
    assert_eq!(signed.sealed_at, unsigned.sealed_at);
    let check = verify(&f);
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(check.rederivation, RederivationCheck::Matches);
    assert!(matches!(
        f.service.sign_attestation(&f.pulse.id),
        Err(PersistenceServiceError::Validation(_))
    ));
}

#[test]
fn a_missing_key_is_never_replaced_without_asking() {
    let (f, keys) = fixture();
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let first_key = status(&f).key_id.unwrap();

    keys.secrets.lock().unwrap().clear();
    assert_eq!(
        f.service.attestation_key_status().unwrap().state,
        AttestationKeyState::KeyMissing
    );
    let sibling = failed_sibling(&f);
    seal_all(&f.service);
    let unsigned = f.service.attestation_status(&sibling).unwrap();
    assert_eq!(unsigned.state, AttestationState::Unsigned);
    assert_eq!(
        f.service.attestation_key_status().unwrap().keys.len(),
        1,
        "no key was created silently"
    );

    let rotated = f.service.create_attestation_key().unwrap();
    assert_eq!(rotated.state, AttestationKeyState::Ready);
    assert_eq!(rotated.keys.len(), 2);
    assert_eq!(rotated.keys[0].state, "active");
    assert_ne!(rotated.keys[0].key_id, first_key);
    assert_eq!(rotated.keys[1].key_id, first_key);
    assert_eq!(rotated.keys[1].state, "retired");
    assert!(rotated.keys[1].retired_at.is_some());
    assert_eq!(
        verify(&f).signature,
        SignatureCheck::Valid,
        "a retired key still verifies what it signed"
    );
    let signed = f.service.sign_attestation(&sibling).unwrap();
    assert_eq!(signed.key_id.as_ref(), Some(&rotated.keys[0].key_id));
}

#[test]
fn a_replaced_key_is_a_mismatch_until_a_new_key_is_created() {
    let (f, keys) = fixture();
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let (_, other) = SigningKey::generate().unwrap();
    keys.secrets
        .lock()
        .unwrap()
        .insert(KEY_CREDENTIAL_REF.into(), other.to_string());
    assert_eq!(
        f.service.attestation_key_status().unwrap().state,
        AttestationKeyState::Mismatch
    );
    let sibling = failed_sibling(&f);
    seal_all(&f.service);
    assert_eq!(
        f.service.attestation_status(&sibling).unwrap().state,
        AttestationState::Unsigned
    );
}

#[test]
fn rotation_keeps_one_active_key_and_every_public_key() {
    let (f, _keys) = fixture();
    assert_eq!(
        f.service.attestation_key_status().unwrap().state,
        AttestationKeyState::NoKey
    );
    f.service.create_attestation_key().unwrap();
    f.service.create_attestation_key().unwrap();
    let keys = f.service.create_attestation_key().unwrap();
    assert_eq!(keys.keys.len(), 3);
    assert_eq!(
        keys.keys.iter().filter(|key| key.state == "active").count(),
        1
    );
    let db = f.service.connection().unwrap();
    let active = keys.keys[0].key_id.clone();
    assert!(
        db.execute(
            "UPDATE attestation_keys SET state='active',retired_at=NULL WHERE key_id<>?1",
            [&active],
        )
        .is_err(),
        "one active key at most"
    );
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    assert!(
        db.execute("DELETE FROM attestation_keys WHERE key_id=?1", [&active])
            .is_err(),
        "a key an attestation names is never deleted"
    );
}

#[test]
fn tampering_with_rows_files_or_the_envelope_is_detected() {
    let (f, _keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "applied");
    publish(&f, "a2", "src/two.rs", "pub fn two() {}\n", "applied");
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);

    std::fs::write(f.workspace.path().join("src/lib.rs"), "edited\n").unwrap();
    std::fs::remove_file(f.workspace.path().join("src/two.rs")).unwrap();
    let check = verify(&f);
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(
        check.rederivation,
        RederivationCheck::Matches,
        "editing a subject changes only the workspace level"
    );
    assert_eq!(
        check
            .workspace
            .iter()
            .map(|subject| subject.state)
            .collect::<Vec<_>>(),
        vec![WorkspaceCheck::Modified, WorkspaceCheck::Missing]
    );

    let db = f.service.connection().unwrap();
    db.execute(
        "INSERT INTO mandate_enforcement_events(id,pulsation_id,stage,rule,relative_path,detail,mandate_revision,created_at,updated_at) VALUES('late',?1,'Code','scope_violation','src/x.rs','refused',1,'now','now')",
        [&f.pulse.id],
    )
    .unwrap();
    let check = verify(&f);
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(check.rederivation, RederivationCheck::Differs);
    assert_eq!(check.difference.as_deref(), Some("conformance"));
    db.execute(
        "UPDATE artifact_publications SET desired_sha256=?2 WHERE artifact_id=?1",
        params!["a2", "0".repeat(64)],
    )
    .unwrap();
    assert_eq!(verify(&f).difference.as_deref(), Some("subject"));

    // Someone with the database file can drop the trigger; the signature
    // still gives the change away.
    db.execute_batch("DROP TRIGGER pulsation_attestations_sign_only")
        .unwrap();
    db.execute(
        "UPDATE pulsation_attestations SET statement_json=replace(statement_json,'\"done\"','\"failed\"') WHERE pulsation_id=?1",
        [&f.pulse.id],
    )
    .unwrap();
    assert_eq!(verify(&f).signature, SignatureCheck::Invalid);
    let envelope: String = db
        .query_row(
            "SELECT envelope_json FROM pulsation_attestations WHERE pulsation_id=?1",
            [&f.pulse.id],
            |row| row.get(0),
        )
        .unwrap();
    let mut parsed: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    let payload = parsed["payload"].as_str().unwrap().to_owned();
    parsed["payload"] = serde_json::Value::String(format!("{}A", &payload[..payload.len() - 1]));
    db.execute(
        "UPDATE pulsation_attestations SET envelope_json=?2 WHERE pulsation_id=?1",
        params![f.pulse.id, parsed.to_string()],
    )
    .unwrap();
    assert_eq!(verify(&f).signature, SignatureCheck::Invalid);
}

#[test]
fn an_attestation_never_changes_except_to_be_signed() {
    let (f, keys) = fixture();
    keys.locked.store(true, Ordering::SeqCst);
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let db = f.service.connection().unwrap();
    let refused = db
        .execute(
            "UPDATE pulsation_attestations SET outcome='failed' WHERE pulsation_id=?1",
            [&f.pulse.id],
        )
        .unwrap_err();
    assert!(refused.to_string().contains("immutable"), "{refused}");
    keys.locked.store(false, Ordering::SeqCst);
    f.service.sign_attestation(&f.pulse.id).unwrap();
    for update in [
        "UPDATE pulsation_attestations SET signature_state='unsigned',envelope_json=NULL,key_id=NULL,signed_at=NULL WHERE pulsation_id=?1",
        "UPDATE pulsation_attestations SET sealed_at='later' WHERE pulsation_id=?1",
        "UPDATE pulsation_attestations SET signed_at='later' WHERE pulsation_id=?1",
    ] {
        assert!(db.execute(update, [&f.pulse.id]).is_err(), "{update}");
    }
}

#[test]
fn a_statement_over_the_size_cap_is_kept_as_a_digest() {
    let (f, _keys) = fixture();
    {
        let mut db = f.service.connection().unwrap();
        let tx = db.transaction().unwrap();
        let long = "d".repeat(180);
        for index in 0..3_000 {
            let path = format!("src/{long}/file_{index:05}.rs");
            let id = format!("a{index}");
            tx.execute(
                "INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,updated_at,producer_stage,acceptance,publication_state)
                 VALUES(?1,?2,?3,'source',?3,?4,'now','now','Code','accepted','applied')",
                params![id, f.pulse.id, path, sha(&path)],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,desired_sha256,state,updated_at) VALUES(?1,?2,?3,?4,?5,'applied','now')",
                params![id, f.pulse.project_id, f.pulse.id, path, sha(&path)],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let sealed = status(&f);
    assert_eq!(sealed.state, AttestationState::TooLarge);
    assert_eq!(sealed.subject_count, 3_000);
    assert!(sealed.statement_sha256.is_some());
    let check = verify(&f);
    assert_eq!(check.signature, SignatureCheck::TooLarge);
    assert_eq!(
        check.rederivation,
        RederivationCheck::Matches,
        "the same build re-derives the same digest"
    );
    assert!(matches!(
        f.service.export_attestation(&f.pulse.id),
        Err(PersistenceServiceError::Validation(_))
    ));
}

/// Only a busy database is worth retrying; a query that cannot run would fail
/// on every turn and hold the background lane.
#[test]
fn a_database_error_other_than_busy_leaves_the_queue() {
    let (f, _keys) = fixture();
    end(&f, PulsationStatus::Done, None);
    f.service
        .connection()
        .unwrap()
        .execute_batch("DROP TABLE mandate_enforcement_events")
        .unwrap();
    assert!(f.service.seal_next_attestation().unwrap());
    let failed = status(&f);
    assert_eq!(failed.state, AttestationState::Failed);
    assert!(
        failed
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("mandate_enforcement_events")),
        "{failed:?}"
    );
    assert!(!f.service.seal_next_attestation().unwrap());

    // Another writer postpones the seal and leaves it pending; reads still
    // go through, so the seal itself meets the busy database.
    let sibling = failed_sibling(&f);
    let blocker = f.service.connection().unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut impatient = f.service.clone();
    impatient.credentials = Arc::new(Keys::default());
    let postponed = std::thread::spawn(move || impatient.seal_next_attestation());
    let result = postponed.join().unwrap();
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        f.service.attestation_status(&sibling).unwrap().state,
        AttestationState::Pending
    );
}

#[test]
fn a_derivation_that_cannot_succeed_is_kept_failed_until_retried() {
    let (f, _keys) = fixture();
    let db = f.service.connection().unwrap();
    db.execute(
        "INSERT INTO secret_sanitization_receipts(call_id,project_id,pulsation_id,policy_identity,input_redaction_count,categories_json,examined_bytes,duration_ms,created_at) VALUES('call',?1,?2,'policy-v1',2,'not json',10,1,'now')",
        params![f.pulse.project_id, f.pulse.id],
    )
    .unwrap();
    end(&f, PulsationStatus::Done, None);
    assert!(f.service.seal_next_attestation().unwrap());
    let failed = status(&f);
    assert_eq!(failed.state, AttestationState::Failed);
    assert!(failed.diagnostic.is_some());
    assert!(
        !f.service.seal_next_attestation().unwrap(),
        "a failed seal is not retried by itself"
    );

    db.execute(
        "UPDATE secret_sanitization_receipts SET categories_json='[\"api_key\"]' WHERE call_id='call'",
        [],
    )
    .unwrap();
    assert_eq!(
        f.service.retry_attestation_seal(&f.pulse.id).unwrap().state,
        AttestationState::Pending
    );
    seal_all(&f.service);
    assert_eq!(status(&f).state, AttestationState::Signed);
    let statement: StatementV1 = serde_json::from_str(&stored_statement(&f)).unwrap();
    assert_eq!(statement.predicate.sanitization.redactions, 2);
    assert_eq!(
        statement.predicate.sanitization.policy_identities,
        vec!["policy-v1".to_owned()]
    );
    assert_eq!(statement.predicate.sanitization.categories.len(), 1);
    assert_eq!(statement.predicate.sanitization.categories[0].count, 1);
}

#[test]
fn a_restart_between_the_end_and_the_seal_resumes_it() {
    let (f, keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "applied");
    end(&f, PulsationStatus::Done, None);
    let data_dir = f.service.database_path().parent().unwrap().to_path_buf();
    let mut restarted = PersistenceService::initialize(&data_dir).unwrap();
    restarted.credentials = keys.clone();
    assert!(restarted.seal_next_attestation().unwrap());
    assert_eq!(
        restarted.attestation_status(&f.pulse.id).unwrap().state,
        AttestationState::Signed
    );
}

#[test]
fn an_exported_attestation_verifies_here_and_elsewhere() {
    let (f, _keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "applied");
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let sealed = status(&f);
    let export = f.service.export_attestation(&f.pulse.id).unwrap();
    assert_eq!(
        export.bundle_file_name,
        format!("{}.intoto.jsonl", f.pulse.id)
    );
    let key_id = sealed.key_id.clone().unwrap();
    assert_eq!(
        export.key_file_name,
        format!(
            "hvc-attestation-key-{}.pub.pem",
            &key_id[crate::attestation::KEY_ID_PREFIX.len()..][..8]
        )
    );
    assert_eq!(export.bundle.lines().count(), 1);
    assert!(export.bundle.ends_with('\n'));
    assert!(
        export
            .public_key_pem
            .starts_with("-----BEGIN PUBLIC KEY-----")
    );

    let here = f
        .service
        .verify_attestation_file(&export.bundle, None)
        .unwrap();
    assert_eq!(here.signature, SignatureCheck::Valid);
    assert_eq!(here.key, FileKey::KnownOnThisMachine);
    assert_eq!(here.key_id.as_ref(), Some(&key_id));
    assert_eq!(here.pulsation_id.as_ref(), Some(&f.pulse.id));
    assert_eq!(here.statement_sha256, sealed.statement_sha256);
    assert_eq!(here.subject_count, 1);

    let (elsewhere, _other_keys) = fixture();
    let there = elsewhere
        .service
        .verify_attestation_file(&export.bundle, Some(&export.public_key_pem))
        .unwrap();
    assert_eq!(there.signature, SignatureCheck::Valid);
    assert_eq!(there.key, FileKey::UnknownKey);
    let without_key = elsewhere
        .service
        .verify_attestation_file(&export.bundle, None)
        .unwrap();
    assert_eq!(without_key.signature, SignatureCheck::UnknownKey);
    assert_eq!(without_key.key, FileKey::Unavailable);

    let mut envelope: serde_json::Value = serde_json::from_str(export.bundle.trim()).unwrap();
    let payload = envelope["payload"].as_str().unwrap().to_owned();
    envelope["payload"] = serde_json::Value::String(format!("{}A", &payload[..payload.len() - 1]));
    let tampered = f
        .service
        .verify_attestation_file(&envelope.to_string(), None)
        .unwrap();
    assert_eq!(tampered.signature, SignatureCheck::Invalid);
    assert_eq!(
        f.service
            .verify_attestation_file("not an envelope", None)
            .unwrap()
            .signature,
        SignatureCheck::Invalid
    );

    assert_eq!(
        f.service.attestation_commit_trailer(&f.pulse.id).unwrap(),
        format!(
            "HVC-Pulsation: {}\nHVC-Attestation: sha256:{}",
            f.pulse.id,
            sealed.statement_sha256.unwrap()
        )
    );
}

/// D6: identifiers, digests, states and counters only.
#[test]
fn the_statement_carries_no_content_names_or_key_material() {
    let (f, keys) = fixture();
    let content = "fn secret() { let token = \"CONTENT-MARKER-51f3\"; }\n";
    publish(&f, "a1", "src/lib.rs", content, "applied");
    let db = f.service.connection().unwrap();
    db.execute(
        "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at) VALUES('cp-risk',?1,'Integrator','approved','now','now')",
        [&f.pulse.id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO pulsation_risk_acknowledgements(id,pulsation_id,checkpoint_id,input_revision,level,pending,reason,created_at) VALUES('ack',?1,'cp-risk',1,'high',1,'REASON-MARKER-77aa','now')",
        [&f.pulse.id],
    )
    .unwrap();
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let statement = stored_statement(&f);
    let parsed: StatementV1 = serde_json::from_str(&statement).unwrap();
    assert_eq!(
        parsed.predicate.evidence.acknowledgements,
        vec![AcknowledgementRefV1 {
            kind: "risk".into(),
            reason_sha256: text_sha256("REASON-MARKER-77aa"),
        }],
        "a reason is kept as its digest"
    );
    let workspace = f.workspace.path().to_string_lossy().into_owned();
    for forbidden in [
        "CONTENT-MARKER-51f3",
        "REASON-MARKER-77aa",
        "Implement a validated contract",
        workspace.as_str(),
        workspace.replace('\\', "/").as_str(),
    ] {
        assert!(!statement.contains(forbidden), "{forbidden} in {statement}");
    }

    // The private key appears in no row of any table.
    let secret = keys.secret().unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for table in tables {
        let columns = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for column in columns {
            let found: i64 = db
                .query_row(
                    &format!("SELECT COUNT(*) FROM \"{table}\" WHERE instr(CAST(\"{column}\" AS TEXT),?1)>0"),
                    [&secret],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            assert_eq!(found, 0, "key material in {table}.{column}");
        }
    }
    let export = f.service.export_attestation(&f.pulse.id).unwrap();
    for text in [&export.bundle, &export.public_key_pem] {
        assert!(!text.contains(secret.as_str()));
    }
    assert!(!export.contents.join("\n").contains(secret.as_str()));
    assert!(
        !format!("{:?}", f.service.attestation_key_status().unwrap()).contains(secret.as_str())
    );
}

#[test]
fn the_deriver_reads_each_block_from_its_rows() {
    let (f, _keys) = fixture();
    publish(&f, "a1", "src/lib.rs", "pub fn one() {}\n", "applied");
    let db = f.service.connection().unwrap();
    let digest = "a".repeat(64);
    db.execute(
        "INSERT INTO pulsation_file_bases(pulsation_id,path_key,relative_path,state,content_sha256,first_artifact_id,captured_at) VALUES(?1,'src/lib.rs','src/lib.rs','present',?2,'a1','now')",
        params![f.pulse.id, digest],
    )
    .unwrap();
    db.execute(
        "INSERT INTO execution_mandates(id,pulsation_id,revision,cause,enforcement,mandate_json,mandate_sha256,checkpoint_id,created_at) VALUES('m1',?1,1,'issued','enforced_v4','{}',?2,'cp','now')",
        params![f.pulse.id, digest],
    )
    .unwrap();
    db.execute(
        "INSERT INTO checkpoint_approval_receipts(checkpoint_id,pulsation_id,purpose,decision,receipt_json,receipt_sha256,mandate_revision,created_at) VALUES('cp',?1,'artifact_review','approved','{\"stage\":\"SPECIFICATION\"}',?2,1,'now')",
        params![f.pulse.id, digest],
    )
    .unwrap();
    db.execute(
        "INSERT INTO pulsation_review_policy_snapshots(pulsation_id,state,files_json,omitted_json,snapshot_sha256,captured_at) VALUES(?1,'absent','[]','[]',?2,'now')",
        params![f.pulse.id, digest],
    )
    .unwrap();
    db.execute(
        "INSERT INTO pulsation_risk_assessments(pulsation_id,stage,input_revision,level,policy_version,policy_sha256,assessment_json,selection_json,created_at) VALUES(?1,'Code',1,'medium',1,?2,'{}','{}','now')",
        params![f.pulse.id, digest],
    )
    .unwrap();
    for (source, quality, output) in [("one", "complete", Some(30)), ("two", "partial", None)] {
        db.execute(
            "INSERT INTO token_usage_records(source_kind,source_id,unit_key,project_id,transport,provider_id,observed_model,input_tokens,output_tokens,quality,started_at,updated_at,pulsation_id) VALUES('llm',?1,'response',?2,'api','openai','gpt-test',10,?3,?4,'now','now',?5)",
            params![source, f.pulse.project_id, output, quality, f.pulse.id],
        )
        .unwrap();
    }
    end(&f, PulsationStatus::Done, None);
    seal_all(&f.service);
    let statement: StatementV1 = serde_json::from_str(&stored_statement(&f)).unwrap();
    let predicate = &statement.predicate;
    assert_eq!(predicate.pulsation.id, f.pulse.id);
    assert_eq!(predicate.pulsation.project_id, f.pulse.project_id);
    assert_eq!(predicate.sealing.deriver_version, DERIVER_VERSION);
    assert_eq!(predicate.sealing.clock, "local");
    assert_eq!(
        predicate.intent.review_policy_snapshot_sha256.as_ref(),
        Some(&digest)
    );
    assert_eq!(predicate.authority.mandates.len(), 1);
    assert_eq!(predicate.authority.mandates[0].cause, "issued");
    assert_eq!(predicate.authority.approvals[0].stage, "SPECIFICATION");
    assert_eq!(predicate.authority.approvals[0].receipt_sha256, digest);
    assert_eq!(predicate.evidence.risk.len(), 1);
    assert_eq!(predicate.evidence.risk[0].level, "medium");
    assert_eq!(predicate.effects[0].base_state.as_deref(), Some("present"));
    assert_eq!(predicate.effects[0].base_sha256.as_ref(), Some(&digest));
    assert_eq!(predicate.effects[0].producer_stage.as_deref(), Some("Code"));
    assert_eq!(predicate.conformance.within_mandate, Some(true));
    assert_eq!(predicate.conformance.enforcement_events, 0);
    assert_eq!(predicate.usage.models.len(), 1);
    assert_eq!(predicate.usage.models[0].input, Some(20));
    assert_eq!(
        predicate.usage.models[0].output, None,
        "an unknown counter stays unknown"
    );
    assert_eq!(predicate.usage.completeness, "partial");

    // The same rows give the same bytes.
    let again = PersistenceService::derive_statement_v1(
        &db,
        &f.pulse.id,
        predicate.builder.clone(),
        &predicate.sealing.sealed_at,
    )
    .unwrap();
    assert_eq!(again.canonical_bytes(), statement.canonical_bytes());
}
