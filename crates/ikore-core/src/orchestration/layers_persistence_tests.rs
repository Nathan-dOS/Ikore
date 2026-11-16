use super::*;
use crate::orchestration::layers::{LayerKind, LayerState};
use crate::orchestration::mandate::{
    self, BudgetParameters, Enforcement, IssueInputs, IssuedBy, StageOutputLimit,
    TransportCapability, WriteClass, WriteGrant,
};
use crate::persistence_service::GetPulsationRuntimeSnapshotRequest;
use crate::persistence_service::serial::tests::Fixture;

/// Makes the fixture's pulsation a v4 one holding an enforced mandate that
/// grants `paths`.
fn compose(fixture: &Fixture, paths: &[&str]) {
    let db = fixture.service.connection().unwrap();
    db.execute(
        "UPDATE pulsations SET pipeline_version=4 WHERE id=?1",
        [&fixture.pulse.id],
    )
    .unwrap();
    let mut issued = mandate::derive_issued(&IssueInputs {
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
        previous: None,
        manifest: None,
        clarification_paths: &[],
        specification: None,
        accepted_artifact_sha256s: &[],
        review_policy_snapshot_sha256: None,
        project_sensitive_sha256: None,
        transports: vec![TransportCapability {
            role: "backend_agent".into(),
            transport: "api".into(),
            backend: None,
        }],
        checks: Vec::new(),
        budget: BudgetParameters {
            stage_output_limits: vec![StageOutputLimit {
                stage: "Code".into(),
                output_tokens: 1_000,
            }],
            context_chars_limit: 1_000,
            extra_invocation_limit: 2,
            per_unit_extra_invocation_limit: 1,
            max_in_flight_units: 1,
        },
    })
    .unwrap();
    issued.scope.grants = paths
        .iter()
        .map(|path| WriteGrant {
            path: (*path).into(),
            origins: vec![WriteClass::TaskPath],
            unit_key: None,
            amendment_revision: None,
        })
        .collect();
    PersistenceService::insert_mandate(&db, &issued).unwrap();
}

fn layer_state(fixture: &Fixture, kind: LayerKind) -> LayerState {
    fixture
        .service
        .pulsation_layers(&fixture.pulse.id)
        .unwrap()
        .layer(kind)
        .unwrap()
        .state
}

#[test]
fn a_legacy_pulsation_reads_without_error() {
    let f = Fixture::new(PulsationStage::Code);
    let layers = f.service.pulsation_layers(&f.pulse.id).unwrap();
    let state = |kind| layers.layer(kind).unwrap().state;
    assert_eq!(state(LayerKind::Scope), LayerState::NotApplicable);
    assert_eq!(state(LayerKind::Intent), LayerState::NotApplicable);
    assert!(layers.worst_flagged().is_none());
}

#[test]
fn a_composed_pulsation_reads_its_mandate_writes_and_refusals() {
    let f = Fixture::new(PulsationStage::Code);
    compose(&f, &["src/auth.rs", "src/lib.rs"]);
    let layers = f.service.pulsation_layers(&f.pulse.id).unwrap();
    let scope = layers.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.state, LayerState::Attention, "{scope:?}");
    assert!(
        scope
            .items
            .iter()
            .any(|item| item.label == "src/auth.rs" && item.value == "granted · sensitive"),
        "{scope:?}"
    );
    let identity = layers.layer(LayerKind::Identity).unwrap();
    assert!(
        identity.headline.ends_with("mandate revision 1"),
        "{identity:?}"
    );

    f.accept(&f.output("advance", Some("src/lib.rs"), "pub fn lib() {}\n"))
        .unwrap();
    f.service.reconcile_publications().unwrap();
    let layers = f.service.pulsation_layers(&f.pulse.id).unwrap();
    let scope = layers.layer(LayerKind::Scope).unwrap();
    assert!(
        scope
            .items
            .iter()
            .any(|item| item.label == "src/lib.rs" && item.value == "applied"),
        "{scope:?}"
    );
    assert_eq!(scope.headline, "2 granted · 1 written · 0 refused");

    let db = f.service.connection().unwrap();
    PersistenceService::record_enforcement_event(
        &db,
        &f.pulse.id,
        "Code",
        "scope_violation",
        Some("Cargo.toml"),
        "refused",
        1,
    )
    .unwrap();
    assert_eq!(layer_state(&f, LayerKind::Scope), LayerState::Issue);
}

#[test]
fn only_the_open_pulsation_carries_its_layers() {
    let f = Fixture::new(PulsationStage::Code);
    let request = || GetPulsationRuntimeSnapshotRequest {
        project_id: f.pulse.project_id.clone(),
    };
    let detail = f
        .service
        .get_pulsation_runtime_snapshot_for_detail(request(), Some(&f.pulse.id))
        .unwrap()
        .snapshot
        .details;
    assert_eq!(detail.len(), 1);
    assert_eq!(
        detail[0].layers.as_ref(),
        Some(&f.service.pulsation_layers(&f.pulse.id).unwrap()),
        "the detail reads the same snapshot"
    );
    let listing = f
        .service
        .get_pulsation_runtime_snapshot(request())
        .unwrap()
        .snapshot
        .details;
    assert!(listing.iter().all(|detail| detail.layers.is_none()));
}

#[test]
fn a_tampered_mandate_is_a_scope_issue_and_the_detail_still_loads() {
    let f = Fixture::new(PulsationStage::Code);
    compose(&f, &["src/lib.rs"]);
    f.service
        .connection()
        .unwrap()
        .execute_batch(
            "DROP TRIGGER execution_mandates_append_only;
             UPDATE execution_mandates SET mandate_json=replace(mandate_json,'src/lib.rs','src/evil.rs');",
        )
        .unwrap();
    let layers = f.service.pulsation_layers(&f.pulse.id).unwrap();
    let scope = layers.layer(LayerKind::Scope).unwrap();
    assert_eq!(scope.state, LayerState::Issue);
    assert_eq!(
        scope.reasons,
        ["The stored mandate does not match its digest."]
    );
}

/// The detail is read in one deferred transaction; the layers must not open
/// another connection inside it (see `runtime_config_from`).
#[test]
fn the_detail_reads_its_layers_while_another_connection_writes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(PulsationStage::Code);
    compose(&f, &["src/lib.rs"]);
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
    let reads = (0..20)
        .map(|_| {
            let started = std::time::Instant::now();
            let read = f.service.get_pulsation_runtime_snapshot_for_detail(
                GetPulsationRuntimeSnapshotRequest {
                    project_id: f.pulse.project_id.clone(),
                },
                Some(&f.pulse.id),
            );
            (
                read.map(|response| response.snapshot.details[0].layers.is_some()),
                started.elapsed(),
            )
        })
        .collect::<Vec<_>>();
    stop.store(true, Ordering::Relaxed);
    assert!(hammer.join().unwrap() > 0, "the writer ran");
    for (read, elapsed) in reads {
        assert!(matches!(read, Ok(true)), "{read:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "a detail read waited {elapsed:?}"
        );
    }
}
