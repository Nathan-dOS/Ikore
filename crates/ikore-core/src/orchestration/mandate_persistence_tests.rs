use super::*;
use crate::orchestration::mandate::{WriteClass, WriteGrant};
use crate::persistence_service::serial::tests::Fixture;
use crate::persistence_service::{HitlDecision, PulsationStatus};
use std::fs;

/// An enforced revision on the fixture's pulsation granting `paths`, chained
/// to the revision in force.
fn enforce(fixture: &Fixture, paths: &[&str]) -> ExecutionMandateV1 {
    let db = fixture.service.connection().unwrap();
    let previous = PersistenceService::current_mandate(&db, &fixture.pulse.id).unwrap();
    let mut mandate = mandate::derive_issued(&IssueInputs {
        pulsation_id: &fixture.pulse.id,
        project_id: &fixture.pulse.project_id,
        enforcement: Enforcement::EnforcedV4,
        title: "Fixture",
        issued_by: IssuedBy {
            checkpoint_id: "cp-fixture".into(),
            stage: "SPECIFICATION".into(),
            state_version: 1,
            input_revision: 1,
        },
        previous: previous.as_ref().map(|(m, sha)| (m, sha.as_str())),
        manifest: None,
        clarification_paths: &[],
        specification: None,
        accepted_artifact_sha256s: &[],
        review_policy_snapshot_sha256: None,
        project_sensitive_sha256: None,
        transports: vec![TransportCapability {
            role: "backend_agent".into(),
            transport: "cli".into(),
            backend: Some("claude".into()),
        }],
        checks: Vec::new(),
        budget: BudgetParameters {
            stage_output_limits: vec![StageOutputLimit {
                stage: "Code".into(),
                output_tokens: 12_345,
            }],
            context_chars_limit: 9_999,
            extra_invocation_limit: 2,
            per_unit_extra_invocation_limit: 1,
            max_in_flight_units: 1,
        },
    })
    .unwrap();
    mandate.scope.grants = paths
        .iter()
        .map(|path| WriteGrant {
            path: (*path).into(),
            origins: vec![WriteClass::TaskPath],
            unit_key: None,
            amendment_revision: None,
        })
        .collect();
    PersistenceService::insert_mandate(&db, &mandate).unwrap();
    mandate
}

fn publication_state(fixture: &Fixture) -> (String, Option<String>) {
    fixture
        .service
        .connection()
        .unwrap()
        .query_row(
            "SELECT state,last_error FROM artifact_publications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn a_stage_write_outside_the_enforced_mandate_is_refused_at_acceptance() {
    let f = Fixture::new(PulsationStage::Code);
    enforce(&f, &["src/allowed.rs"]);
    let error = f
        .accept(&f.output("advance", Some("src/other.rs"), "pub fn other() {}\n"))
        .unwrap_err();
    match error {
        PersistenceServiceError::Validation(reason) => {
            assert!(reason.starts_with("mandate_scope_violation:"), "{reason}")
        }
        other => panic!("expected a host-contract refusal, got {other:?}"),
    }
    assert_eq!(f.count("artifacts"), 0, "nothing was accepted");
    assert_eq!(f.count("artifact_publications"), 0);
    assert!(!f.workspace.path().join("src/other.rs").exists());
}

#[test]
fn a_write_inside_the_mandate_passes_both_checks() {
    let f = Fixture::new(PulsationStage::Code);
    enforce(&f, &["SRC/New.rs"]);
    f.accept(&f.output("advance", Some("src/new.rs"), "pub fn new() {}\n"))
        .unwrap();
    f.service.reconcile_publications().unwrap();
    assert_eq!(publication_state(&f).0, "applied");
    assert_eq!(
        fs::read_to_string(f.workspace.path().join("src/new.rs")).unwrap(),
        "pub fn new() {}\n"
    );
    assert!(
        f.service
            .mandate_enforcement_events(&f.pulse.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_publication_outside_the_revision_in_force_is_a_conflict_and_an_event() {
    let f = Fixture::new(PulsationStage::Code);
    enforce(&f, &["code.rs"]);
    f.accept(&f.output("advance", Some("code.rs"), "generated\n"))
        .unwrap();
    // A later revision no longer grants the file before it is written.
    let revised = enforce(&f, &[]);
    assert_eq!(revised.revision, 2);
    f.service.reconcile_publications().unwrap();
    let (state, error) = publication_state(&f);
    assert_eq!(state, "conflict");
    assert!(
        error
            .as_deref()
            .is_some_and(|text| text.contains("mandate_scope_violation")),
        "{error:?}"
    );
    assert!(
        !f.workspace.path().join("code.rs").exists(),
        "nothing written"
    );
    let events = f.service.mandate_enforcement_events(&f.pulse.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].rule, "scope_violation");
    assert_eq!(events[0].relative_path.as_deref(), Some("code.rs"));
    assert_eq!(events[0].mandate_revision, 2);
}

#[test]
fn a_legacy_mandate_records_without_refusing() {
    let f = Fixture::new(PulsationStage::Code);
    let db = f.service.connection().unwrap();
    let first = enforce(&f, &[]);
    let mut legacy = first.clone();
    legacy.revision = 2;
    legacy.previous_sha256 = Some(first.sha256().unwrap().as_str().to_owned());
    legacy.enforcement = Enforcement::NotAppliedLegacy;
    PersistenceService::insert_mandate(&db, &legacy).unwrap();
    f.accept(&f.output("advance", Some("anything.rs"), "x\n"))
        .unwrap();
    f.service.reconcile_publications().unwrap();
    assert_eq!(publication_state(&f).0, "applied");
    assert_eq!(
        PersistenceService::frozen_budget(&db, &f.pulse.id).unwrap(),
        None,
        "a legacy pipeline keeps its live budget"
    );
}

#[test]
fn revisions_are_append_only_and_verified_against_their_digest() {
    let f = Fixture::new(PulsationStage::Code);
    enforce(&f, &["a.rs"]);
    let db = f.service.connection().unwrap();
    assert!(
        db.execute("UPDATE execution_mandates SET cause='scope_grant'", [])
            .is_err(),
        "the trigger refuses an update"
    );
    db.execute_batch(
        "DROP TRIGGER execution_mandates_append_only;
         UPDATE execution_mandates SET mandate_json=replace(mandate_json,'a.rs','b.rs');",
    )
    .unwrap();
    let error = PersistenceService::current_mandate(&db, &f.pulse.id).unwrap_err();
    assert!(
        error.to_string().contains("mandate_integrity_failed"),
        "{error}"
    );
}

#[test]
fn approving_the_specification_issues_the_previewed_mandate_and_a_receipt() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "approved specification\n"))
        .unwrap();
    let checkpoint = f.service.get_hitl_checkpoint(&f.pulse.id).unwrap();
    let preview = f
        .service
        .mandate_preview(&checkpoint.checkpoint_id)
        .unwrap()
        .expect("a waiting Specification review has a preview");
    assert_eq!(preview.mandate.enforcement, Enforcement::NotAppliedLegacy);
    assert!(preview.mandate.scope.grants.is_empty());
    let candidates: Vec<(String, String)> = f
        .service
        .connection()
        .unwrap()
        .prepare("SELECT id,content_sha256 FROM artifacts WHERE acceptance='candidate' ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    let revisions = f.service.pulsation_mandates(&f.pulse.id).unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].cause, MandateCause::Issued);
    assert_eq!(
        revisions[0].mandate, preview.mandate,
        "the preview is what was issued"
    );
    assert_eq!(revisions[0].mandate_sha256, preview.mandate_sha256);
    assert_eq!(
        revisions[0].checkpoint_id.as_deref(),
        Some(checkpoint.checkpoint_id.as_str())
    );
    let receipt = f
        .service
        .checkpoint_receipt(&checkpoint.checkpoint_id)
        .unwrap()
        .expect("the decision has a receipt");
    assert_eq!(receipt.purpose, "artifact_review");
    assert_eq!(receipt.decision, "approved");
    assert_eq!(receipt.mandate_revision, Some(1));
    assert_eq!(receipt.state_version, checkpoint.state_version);
    assert_eq!(
        receipt
            .reviewed_artifacts
            .iter()
            .map(|a| (a.artifact_id.clone(), a.sha256.clone()))
            .collect::<Vec<_>>(),
        candidates
    );
    assert_eq!(
        receipt.feedback_sha256,
        Some(text_sha256("Revisar com evidência")),
        "feedback is kept as a digest only"
    );
    assert!(
        f.service
            .mandate_preview(&checkpoint.checkpoint_id)
            .unwrap()
            .is_none(),
        "a decided checkpoint has no preview"
    );
}

/// The preview reads on its own transaction's connection only. With the
/// rollback journal, a second connection opened inside that read transaction
/// waits for a writer that is itself waiting for the read to end, until the
/// busy timeout fails the preview with `database is locked`.
#[test]
fn the_preview_reads_while_another_connection_writes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "specification\n"))
        .unwrap();
    let checkpoint = f.service.get_hitl_checkpoint(&f.pulse.id).unwrap();
    let writer = f.service.connection().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let hammer = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut writes = 0u32;
            while !stop.load(Ordering::Relaxed) {
                if writer
                    .execute(
                        "INSERT INTO runtime_settings(key,value,updated_at) VALUES('test.writer',?1,'now') ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                        [writes],
                    )
                    .is_ok()
                {
                    writes += 1;
                }
            }
            writes
        })
    };
    let previews = (0..20)
        .map(|_| {
            let started = std::time::Instant::now();
            let preview = f.service.mandate_preview(&checkpoint.checkpoint_id);
            (preview, started.elapsed())
        })
        .collect::<Vec<_>>();
    stop.store(true, Ordering::Relaxed);
    assert!(hammer.join().unwrap() > 0, "the writer ran");
    for (preview, elapsed) in previews {
        assert!(matches!(preview, Ok(Some(_))), "{preview:?}");
        // The busy timeout is 5 s; waiting it out means a lock cycle.
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "a preview waited {elapsed:?}"
        );
    }
}

#[test]
fn approving_the_specification_again_reissues_on_the_chain() {
    let f = Fixture::new(PulsationStage::Specification);
    // A revision back to the Specification after an earlier approval.
    let first = enforce(&f, &["src/old.rs"]);
    f.accept(&f.output("advance", None, "revised specification\n"))
        .unwrap();
    let checkpoint = f.service.get_hitl_checkpoint(&f.pulse.id).unwrap();
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Approve))
        .unwrap();
    let revisions = f.service.pulsation_mandates(&f.pulse.id).unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[1].cause, MandateCause::Reissued);
    assert_eq!(revisions[1].mandate.revision, 2);
    assert_eq!(
        revisions[1].mandate.previous_sha256.as_deref(),
        Some(first.sha256().unwrap().as_str())
    );
    assert_eq!(
        f.service
            .checkpoint_receipt(&checkpoint.checkpoint_id)
            .unwrap()
            .unwrap()
            .mandate_revision,
        Some(2),
        "the receipt names the revision the approval issued"
    );
}

#[test]
fn a_rejection_gets_a_receipt_and_no_mandate() {
    let f = Fixture::new(PulsationStage::Specification);
    f.accept(&f.output("advance", None, "draft\n")).unwrap();
    let checkpoint = f.service.get_hitl_checkpoint(&f.pulse.id).unwrap();
    f.service
        .apply_hitl_decision(f.decision(HitlDecision::Reject))
        .unwrap();
    assert_eq!(f.state().status, PulsationStatus::Failed);
    assert!(
        f.service
            .pulsation_mandates(&f.pulse.id)
            .unwrap()
            .is_empty()
    );
    let receipt = f
        .service
        .checkpoint_receipt(&checkpoint.checkpoint_id)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.decision, "rejected");
    assert_eq!(receipt.mandate_revision, None);
}

#[test]
fn an_api_fallback_amends_the_transport_of_its_role() {
    let f = Fixture::new(PulsationStage::Code);
    let first = enforce(&f, &[]);
    let db = f.service.connection().unwrap();
    let revision = PersistenceService::amend_mandate_transport(
        &db,
        &f.pulse.id,
        AgentRole::BackendAgent,
        "cp-fallback",
        PulsationStage::Code,
        3,
    )
    .unwrap();
    assert_eq!(revision, Some(2));
    let (current, _) = PersistenceService::current_mandate(&db, &f.pulse.id)
        .unwrap()
        .unwrap();
    assert_eq!(current.cause, MandateCause::TransportFallback);
    assert_eq!(
        current.previous_sha256.as_deref(),
        Some(first.sha256().unwrap().as_str())
    );
    assert!(mandate::check_transport(&current, "backend_agent", "api", None).is_ok());
}

#[test]
fn the_budget_and_capabilities_come_from_the_mandate_in_force() {
    let f = Fixture::new(PulsationStage::Code);
    let db = f.service.connection().unwrap();
    assert_eq!(
        PersistenceService::frozen_budget(&db, &f.pulse.id).unwrap(),
        None
    );
    enforce(&f, &[]);
    let budget = PersistenceService::frozen_budget(&db, &f.pulse.id)
        .unwrap()
        .unwrap();
    assert_eq!(budget.output_limit_for("Code"), Some(12_345));
    assert_eq!(budget.context_chars_limit, 9_999);
    let facts = |transport: &'static str, backend: Option<&'static str>| AdmissionFacts {
        stage: comp::Stage::Code,
        role: AgentRole::BackendAgent,
        transports: vec![(ExecutorKind::CodeImplementer, transport, backend)],
        applicable_checks: &[],
    };
    assert_eq!(
        PersistenceService::mandate_admission_refusal(
            &db,
            &f.pulse.id,
            &facts("cli", Some("claude"))
        )
        .unwrap(),
        None
    );
    let refusal = PersistenceService::mandate_admission_refusal(
        &db,
        &f.pulse.id,
        &facts("cli", Some("codex")),
    )
    .unwrap()
    .expect("a changed transport is refused");
    assert!(
        refusal.starts_with("mandate_capability_mismatch:"),
        "{refusal}"
    );
}

#[test]
fn enforcement_events_are_bounded_per_pulsation() {
    let f = Fixture::new(PulsationStage::Code);
    let db = f.service.connection().unwrap();
    for index in 0..(MAX_ENFORCEMENT_EVENTS + 5) {
        PersistenceService::record_enforcement_event(
            &db,
            &f.pulse.id,
            "Code",
            "scope_violation",
            Some(&format!("f{index}.rs")),
            "refused",
            1,
        )
        .unwrap();
    }
    let events = f.service.mandate_enforcement_events(&f.pulse.id).unwrap();
    assert_eq!(events.len() as i64, MAX_ENFORCEMENT_EVENTS);
    assert_eq!(events.iter().map(|e| e.occurrences).sum::<u32>(), 205);
}
