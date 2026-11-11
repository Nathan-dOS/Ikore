//! ADR-036 M3 over the real pipeline v4: a composed pulsation that finishes
//! is sealed and signed by the background lane, its statement names what the
//! host recorded, and its bundle verifies here and on another machine; one
//! aborted after writing is sealed with its effects and `outcome: failed`.
//! Every service here keeps its key in memory; the opt-in test at the end is
//! the only one that reaches the Windows Credential Manager, under a
//! test-only service name.
#![allow(
    clippy::unwrap_used,
    reason = "Test-only integration helpers fail immediately when fixture setup is invalid"
)]
mod composition_support;

use std::sync::Arc;

use composition_support::*;
use hvc_core::AppCoreDependencies;
use hvc_core::attestation::StatementV1;
use hvc_core::llm::{CredentialStore, LlmError, LlmErrorKind};
use hvc_core::persistence_service::*;
use hvc_test_support::{FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore};
use serde_json::json;

fn wait_for_seal(harness: &Harness) -> AttestationStatus {
    wait_until(&harness.service, &harness.pulse, |_| {
        !matches!(
            harness
                .service
                .attestation_status(&harness.pulse)
                .unwrap()
                .state,
            AttestationState::NotSealed | AttestationState::Pending
        )
    });
    harness.service.attestation_status(&harness.pulse).unwrap()
}

fn statement(harness: &Harness) -> StatementV1 {
    let json: String = harness
        .db()
        .query_row(
            "SELECT statement_json FROM pulsation_attestations WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

/// Lower-cased relative path and applied digest of each path the
/// pulsation's main publications applied last.
fn applied(harness: &Harness) -> Vec<(String, String)> {
    let mut last = std::collections::BTreeMap::new();
    for row in harness
        .db()
        .prepare(
            "SELECT relative_path,desired_sha256 FROM artifact_publications WHERE pulsation_id=?1 AND job_class='main' AND state='applied' ORDER BY updated_at,rowid",
        )
        .unwrap()
        .query_map([&harness.pulse], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
    {
        let (path, digest) = row.unwrap();
        last.insert(path.replace('\\', "/").to_ascii_lowercase(), digest);
    }
    last.into_iter().collect()
}

fn another_machine() -> (IsolatedTestEnvironment, PersistenceService) {
    let env = IsolatedTestEnvironment::new();
    let deps = AppCoreDependencies::default();
    let service = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        Arc::new(MemoryCredentialStore::default()),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    (env, service)
}

#[test]
fn a_composed_pulsation_that_finishes_is_sealed_signed_and_verifiable() {
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    for stage in ["SPECIFICATION", "Contract", "Test", "Integrator"] {
        harness.wait_for_checkpoint(stage);
        harness.approve();
    }
    let sealed = wait_for_seal(&harness);
    assert_eq!(sealed.state, AttestationState::Signed, "{sealed:?}");
    assert_eq!(sealed.outcome.as_deref(), Some("done"));

    let statement = statement(&harness);
    let subjects = statement
        .subject
        .iter()
        .map(|subject| {
            (
                subject.name.to_ascii_lowercase(),
                subject.digest.sha256.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert!(!subjects.is_empty());
    assert_eq!(subjects, applied(&harness), "one subject per applied path");
    assert_eq!(sealed.subject_count, subjects.len());
    let predicate = &statement.predicate;
    assert_eq!(predicate.pulsation.pipeline_version, 4);
    assert_eq!(predicate.authority.mandates[0].revision, 1);
    assert!(
        predicate.authority.approvals.len() >= 4,
        "every checkpoint decision has its receipt: {:?}",
        predicate.authority.approvals
    );
    assert!(
        predicate
            .execution
            .stages
            .iter()
            .any(|stage| stage.stage == "Code" && !stage.attempts.is_empty()),
        "{:?}",
        predicate.execution.stages
    );
    assert!(
        predicate
            .execution
            .stages
            .iter()
            .flat_map(|stage| &stage.attempts)
            .all(|attempt| attempt.observed_model.is_some()),
        "every attempt names the model that ran"
    );
    assert!(predicate.intent.specification_sha256.is_some());
    assert_eq!(predicate.conformance.within_mandate, Some(true));
    assert_eq!(predicate.effects.len(), subjects.len());
    assert!(predicate.evidence.integrator_manifest.is_some());

    let check = harness
        .service
        .verify_attestation(&harness.pulse)
        .unwrap()
        .unwrap();
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(
        check.rederivation,
        RederivationCheck::Matches,
        "{:?}",
        check.difference
    );
    assert!(
        check
            .workspace
            .iter()
            .all(|subject| subject.state == WorkspaceCheck::Unchanged),
        "{:?}",
        check.workspace
    );

    let report = harness
        .service
        .write_pulsation_report(&harness.pulse)
        .unwrap();
    let html = std::fs::read_to_string(report).unwrap();
    assert!(html.contains("id=\"provenance\""));
    assert!(html.contains(&format!(
        "sha256:{}",
        sealed.statement_sha256.as_deref().unwrap()
    )));

    // Edited after the seal: only the workspace level notices.
    let edited = &statement.subject[0].name;
    std::fs::write(harness.workspace().join(edited), "edited by hand\n").unwrap();
    let check = harness
        .service
        .verify_attestation(&harness.pulse)
        .unwrap()
        .unwrap();
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(check.rederivation, RederivationCheck::Matches);
    assert_eq!(check.workspace[0].state, WorkspaceCheck::Modified);

    let export = harness.service.export_attestation(&harness.pulse).unwrap();
    // For checking a real bundle with OpenSSL as `docs/attestation.md` says.
    if let Ok(directory) = std::env::var("HVC_ATTESTATION_OUT") {
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join(&export.bundle_file_name), &export.bundle).unwrap();
        std::fs::write(
            directory.join(&export.key_file_name),
            &export.public_key_pem,
        )
        .unwrap();
    }
    let (_elsewhere_env, elsewhere) = another_machine();
    let imported = elsewhere
        .verify_attestation_file(&export.bundle, Some(&export.public_key_pem))
        .unwrap();
    assert_eq!(imported.signature, SignatureCheck::Valid);
    assert_eq!(imported.key, FileKey::UnknownKey);
    assert_eq!(imported.pulsation_id.as_ref(), Some(&harness.pulse));
    assert_eq!(imported.statement_sha256, sealed.statement_sha256);
}

#[test]
fn a_pulsation_aborted_after_writing_is_sealed_as_failed_with_its_effects() {
    // Both Code specialties answer with an empty payload, so Code parks on a
    // failure checkpoint after the Specification and Contract wrote their
    // notes; the developer aborts.
    let harness = Harness::start(|facts, _, _| {
        if matches!(
            facts.executor.as_str(),
            "code_implementer" | "code_repairer"
        ) {
            provider_reply(json!({
                "schemaVersion": 1, "outcome": "completed",
                "executorKind": facts.executor, "payload": {}
            }))
        } else {
            completed_reply(facts)
        }
    });
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.approve();
    harness.wait_for_checkpoint("Contract");
    harness.approve();
    let checkpoint = harness.wait_for_failure_checkpoint();
    harness
        .service
        .resolve_stage_failure(ResolveStageFailureRequest {
            checkpoint_id: checkpoint,
            expected_state_version: harness.state_version(),
            decision: StageFailureDecision::Abort,
            feedback: None,
        })
        .unwrap();

    let sealed = wait_for_seal(&harness);
    assert_eq!(sealed.state, AttestationState::Signed, "{sealed:?}");
    assert_eq!(sealed.outcome.as_deref(), Some("failed"));
    let statement = statement(&harness);
    let written = applied(&harness);
    assert!(!written.is_empty(), "the earlier stages wrote their notes");
    assert_eq!(statement.subject.len(), written.len());
    assert_eq!(statement.predicate.effects.len(), written.len());
    assert!(statement.predicate.pulsation.terminal_reason.is_some());
    assert!(statement.predicate.pulsation.ended_at.is_some());
    let check = harness
        .service
        .verify_attestation(&harness.pulse)
        .unwrap()
        .unwrap();
    assert_eq!(check.signature, SignatureCheck::Valid);
    assert_eq!(check.rederivation, RederivationCheck::Matches);
}

/// The operating system's store under a service name the app never uses.
struct TestOnlyKeyring;

impl TestOnlyKeyring {
    const SERVICE: &'static str = "ikore.attestation-test";

    fn entry(credential_ref: &str) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(Self::SERVICE, credential_ref)
    }
}

fn store_error(error: keyring::Error) -> LlmError {
    LlmError::new(LlmErrorKind::Persistence, error.to_string())
}

impl CredentialStore for TestOnlyKeyring {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError> {
        match Self::entry(credential_ref)
            .map_err(store_error)?
            .get_password()
        {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }

    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        Self::entry(credential_ref)
            .and_then(|entry| entry.set_password(secret))
            .map_err(store_error)
    }

    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError> {
        match Self::entry(credential_ref)
            .map_err(store_error)?
            .delete_credential()
        {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(store_error(error)),
        }
    }
}

#[test]
#[ignore = "Writes to the Windows Credential Manager under a test-only service name and removes the entry afterwards"]
fn the_credential_manager_keeps_creates_and_rotates_the_key() {
    let env = IsolatedTestEnvironment::new();
    let store = Arc::new(TestOnlyKeyring);
    let reference = hvc_core::attestation::KEY_CREDENTIAL_REF;
    store.delete(reference).unwrap();
    let deps = AppCoreDependencies::default();
    let service = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        store.clone(),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_eq!(
            service.attestation_key_status().unwrap().state,
            AttestationKeyState::NoKey
        );
        let created = service.create_attestation_key().unwrap();
        assert_eq!(created.state, AttestationKeyState::Ready);
        assert!(store.get(reference).unwrap().is_some());
        let rotated = service.create_attestation_key().unwrap();
        assert_eq!(rotated.state, AttestationKeyState::Ready);
        assert_eq!(rotated.keys.len(), 2);
        assert_ne!(rotated.keys[0].key_id, created.keys[0].key_id);
    }));
    store.delete(reference).unwrap();
    outcome.unwrap();
}
