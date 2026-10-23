use super::tests::test_service;
use super::*;
use crate::test_support::TestDatabase;
use std::cell::Cell;

const OFFICIAL_BACKENDS: [CliBackend; 5] = [
    CliBackend::Codex,
    CliBackend::Claude,
    CliBackend::Gemini,
    CliBackend::Opencode,
    CliBackend::Antigravity,
];

const UNAVAILABLE_STATES: [CliAvailabilityState; 7] = [
    CliAvailabilityState::Missing,
    CliAvailabilityState::Unsupported,
    CliAvailabilityState::AuthRequired,
    CliAvailabilityState::Probing,
    CliAvailabilityState::UnsupportedVersion,
    CliAvailabilityState::CapabilityMissing,
    CliAvailabilityState::ProbeFailed,
];

struct NoopTerminalEvents;

impl TerminalEventSink for NoopTerminalEvents {
    fn emit(&self, _: &TerminalEventEnvelope) {}
}

fn service() -> (TerminalService, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let service = test_service(
        persistence.database_path(),
        Handle::current(),
        Arc::new(NoopTerminalEvents),
    )
    .unwrap();
    (service, data)
}

fn availability(
    backend: CliBackend,
    state: CliAvailabilityState,
    revision: &str,
) -> CliAvailability {
    let (install_url, install_command) = install_guidance(backend);
    CliAvailability {
        backend,
        state,
        executable_path: (state != CliAvailabilityState::Missing)
            .then(|| PathBuf::from(format!("fixture-{}-{revision}.exe", backend.as_str()))),
        version: Some(format!("fixture-{revision}")),
        diagnostic: (state != CliAvailabilityState::Available)
            .then(|| format!("fixture {revision}: {state:?}")),
        install_url,
        install_command,
        capabilities: adapters::capabilities(backend, None),
        quota: Vec::new(),
    }
}

#[tokio::test]
async fn fresh_unavailable_backends_are_rechecked_and_recovery_is_cached() {
    let (service, _data) = service();
    for backend in OFFICIAL_BACKENDS {
        for state in UNAVAILABLE_STATES {
            store_cli_discovery(
                &service.discovery_cache,
                vec![availability(backend, state, "before-login-or-repair")],
            );
            let scans = Cell::new(0);
            let recovered = vec![availability(
                backend,
                CliAvailabilityState::Available,
                "after-login-or-repair",
            )];
            let launched = service
                .discovery_for_launch_with(backend, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(recovered.clone())
                })
                .await;
            assert_eq!(launched, recovered, "{backend:?} recovering from {state:?}");
            assert_eq!(scans.get(), 1, "a fresh {state:?} entry must be rechecked");
            assert_eq!(service.cache_if_fresh(), Some(recovered.clone()));

            let launched_again = service
                .discovery_for_launch_with(backend, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(Vec::new())
                })
                .await;
            assert_eq!(launched_again, recovered);
            assert_eq!(scans.get(), 1, "a recovered backend should reuse its cache");
        }
    }
}

#[tokio::test]
async fn unsuccessful_rechecks_remain_unavailable_and_the_next_retry_can_recover() {
    let (service, _data) = service();
    for backend in OFFICIAL_BACKENDS {
        store_cli_discovery(
            &service.discovery_cache,
            vec![availability(
                backend,
                CliAvailabilityState::AuthRequired,
                "initial",
            )],
        );
        let scans = Cell::new(0);
        let still_unavailable = vec![availability(
            backend,
            CliAvailabilityState::AuthRequired,
            "not-signed-in-yet",
        )];
        let refused = service
            .discovery_for_launch_with(backend, || {
                scans.set(scans.get() + 1);
                std::future::ready(still_unavailable.clone())
            })
            .await;
        assert_eq!(refused, still_unavailable, "{backend:?}");
        assert_eq!(service.cache_if_fresh(), Some(still_unavailable));
        assert_eq!(scans.get(), 1);

        let recovered = vec![availability(
            backend,
            CliAvailabilityState::Available,
            "signed-in",
        )];
        let retried = service
            .discovery_for_launch_with(backend, || {
                scans.set(scans.get() + 1);
                std::future::ready(recovered.clone())
            })
            .await;
        assert_eq!(retried, recovered, "{backend:?}");
        assert_eq!(
            scans.get(),
            2,
            "an unsuccessful recheck cannot block a retry"
        );
    }
}

#[tokio::test]
async fn a_backend_missing_from_a_fresh_snapshot_is_discovered_before_launch() {
    let (service, _data) = service();
    for backend in OFFICIAL_BACKENDS {
        let other_backend = OFFICIAL_BACKENDS
            .into_iter()
            .find(|other| *other != backend)
            .unwrap();
        let other = availability(other_backend, CliAvailabilityState::Available, "other");
        for initial in [Vec::new(), vec![other.clone()]] {
            store_cli_discovery(&service.discovery_cache, initial);
            let scans = Cell::new(0);
            let discovered = vec![
                other.clone(),
                availability(backend, CliAvailabilityState::Available, "newly-installed"),
            ];
            let launched = service
                .discovery_for_launch_with(backend, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(discovered.clone())
                })
                .await;
            assert_eq!(launched, discovered, "{backend:?}");
            assert_eq!(scans.get(), 1);
            assert_eq!(service.cache_if_fresh(), Some(discovered));
        }
    }
}

#[tokio::test]
async fn fresh_healthy_selection_does_not_reprobe_for_other_unavailable_backends() {
    let (service, _data) = service();
    for backend in OFFICIAL_BACKENDS {
        for state in UNAVAILABLE_STATES {
            let cached: Vec<_> = OFFICIAL_BACKENDS
                .into_iter()
                .map(|other| {
                    availability(
                        other,
                        if other == backend {
                            CliAvailabilityState::Available
                        } else {
                            state
                        },
                        "cached",
                    )
                })
                .collect();
            store_cli_discovery(&service.discovery_cache, cached.clone());
            let scans = Cell::new(0);
            let launched = service
                .discovery_for_launch_with(backend, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(Vec::new())
                })
                .await;
            assert_eq!(launched, cached, "{backend:?} with other CLIs {state:?}");
            assert_eq!(
                scans.get(),
                0,
                "only the selected backend needs to be ready"
            );
        }
    }
}

#[tokio::test]
async fn launch_without_a_discovery_snapshot_scans_and_caches_the_result() {
    let (service, _data) = service();
    let expected = vec![availability(
        CliBackend::Claude,
        CliAvailabilityState::Available,
        "initial-discovery",
    )];
    let scans = Cell::new(0);
    let launched = service
        .discovery_for_launch_with(CliBackend::Claude, || {
            scans.set(scans.get() + 1);
            std::future::ready(expected.clone())
        })
        .await;
    assert_eq!(launched, expected);
    assert_eq!(scans.get(), 1);
    assert_eq!(service.cache_if_fresh(), Some(expected));
}

#[tokio::test]
async fn setup_failures_invalidate_only_the_failed_backend_and_retry_rediscovers_it() {
    let (service, _data) = service();
    for backend in OFFICIAL_BACKENDS {
        let other_backend = OFFICIAL_BACKENDS
            .into_iter()
            .find(|other| *other != backend)
            .unwrap();
        let other = availability(other_backend, CliAvailabilityState::Available, "other");
        let setup_errors = [
            TerminalError::AuthRequired(backend.label().into()),
            TerminalError::VersionUnsupported(backend.label().into()),
            TerminalError::NotFound(format!("{} CLI", backend.label())),
            TerminalError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture executable moved",
            )),
            TerminalError::Security("fixture executable identity changed".into()),
        ];
        for error in setup_errors {
            store_cli_discovery(
                &service.discovery_cache,
                vec![
                    availability(backend, CliAvailabilityState::Available, "before-failure"),
                    other.clone(),
                ],
            );
            service.invalidate_discovery_after_failure(backend, &error);
            assert_eq!(
                service.cache_if_fresh(),
                Some(vec![other.clone()]),
                "{backend:?}: {error}"
            );

            let scans = Cell::new(0);
            let recovered = vec![
                availability(backend, CliAvailabilityState::Available, "repaired-path"),
                other.clone(),
            ];
            let retried = service
                .discovery_for_launch_with(backend, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(recovered.clone())
                })
                .await;
            assert_eq!(retried, recovered, "{backend:?}: {error}");
            assert_eq!(scans.get(), 1);
            assert_eq!(service.cache_if_fresh(), Some(recovered));
        }
    }
}

#[tokio::test]
async fn non_setup_failures_preserve_the_discovery_cache() {
    let (service, _data) = service();
    for backend in OFFICIAL_BACKENDS {
        let cached: Vec<_> = OFFICIAL_BACKENDS
            .into_iter()
            .map(|item| availability(item, CliAvailabilityState::Available, "cached"))
            .collect();
        let unrelated_errors = [
            TerminalError::Validation("fixture invalid configuration".into()),
            TerminalError::Conflict,
            TerminalError::Database(rusqlite::Error::InvalidQuery),
            TerminalError::Json(serde_json::from_str::<Value>("{").unwrap_err()),
            TerminalError::Process("fixture transient failure".into()),
            TerminalError::Timeout,
            TerminalError::InactivityTimeout,
            TerminalError::OutputLimit,
            TerminalError::OutputBudgetExceeded {
                observed: 25_000,
                ceiling: 24_000,
            },
            TerminalError::ModelMismatch {
                expected: "selected-model".into(),
                observed: "other-model".into(),
            },
            TerminalError::ModelUnverified {
                detail: "fixture unverified model".into(),
            },
            TerminalError::OutputTruncated {
                backend: backend.label().into(),
                reason: "max_tokens".into(),
                produced_chars: 10,
            },
            TerminalError::Cancelled,
            TerminalError::RateLimited(backend.label().into()),
            TerminalError::InvalidTransition {
                from: "failed".into(),
                to: "running".into(),
            },
            TerminalError::Redaction("fixture redaction error".into()),
        ];
        for error in unrelated_errors {
            store_cli_discovery(&service.discovery_cache, cached.clone());
            service.invalidate_discovery_after_failure(backend, &error);
            assert_eq!(
                service.cache_if_fresh(),
                Some(cached.clone()),
                "{backend:?}: {error}"
            );
        }
    }
}

struct StageFixture {
    service: TerminalService,
    project_id: String,
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
}

fn stage_fixture() -> StageFixture {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = TestDatabase::initialize(data.path()).unwrap();
    let project = persistence
        .insert_project(
            workspace.path().to_string_lossy().into_owned(),
            Some("CLI recovery preflight".into()),
        )
        .unwrap();
    let service = test_service(
        persistence.database_path(),
        Handle::current(),
        Arc::new(NoopTerminalEvents),
    )
    .unwrap();
    service
        .repository
        .ensure_project_configs(&project.id, false)
        .unwrap();
    StageFixture {
        service,
        project_id: project.id,
        _data: data,
        _workspace: workspace,
    }
}

fn configure_role(
    fixture: &StageFixture,
    role: AgentRole,
    mode: AgentExecutionMode,
    backend: CliBackend,
) {
    assert_eq!(
        fixture
            .service
            .repository
            .connection()
            .unwrap()
            .execute(
                "UPDATE cli_agent_configs SET execution_mode=?1,backend=?2,
                    custom_executable=?3,custom_risk_ack=?4
                 WHERE project_id=?5 AND agent_role=?6",
                params![
                    mode.as_str(),
                    backend.as_str(),
                    (backend == CliBackend::Custom).then_some("fixture-custom.exe"),
                    backend == CliBackend::Custom,
                    fixture.project_id,
                    role.as_str(),
                ],
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn stage_dispatch_rechecks_every_unavailable_backend_with_one_scan() {
    let fixture = stage_fixture();
    for (index, (_, _, role)) in hvc_contracts::STAGES.into_iter().enumerate() {
        configure_role(
            &fixture,
            role,
            AgentExecutionMode::CliProcess,
            OFFICIAL_BACKENDS[index % OFFICIAL_BACKENDS.len()],
        );
    }
    for backend in OFFICIAL_BACKENDS {
        for state in UNAVAILABLE_STATES {
            let cached = OFFICIAL_BACKENDS
                .into_iter()
                .map(|item| {
                    availability(
                        item,
                        if item == backend {
                            state
                        } else {
                            CliAvailabilityState::Available
                        },
                        "before-repair",
                    )
                })
                .collect();
            store_cli_discovery(&fixture.service.discovery_cache, cached);
            let recovered: Vec<_> = OFFICIAL_BACKENDS
                .into_iter()
                .map(|item| availability(item, CliAvailabilityState::Available, "repaired"))
                .collect();
            let scans = Cell::new(0);
            fixture
                .service
                .refresh_stage_discovery_with(&fixture.project_id, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(recovered.clone())
                })
                .await
                .unwrap();
            assert_eq!(scans.get(), 1, "{backend:?}: {state:?}");
            assert_eq!(fixture.service.cache_if_fresh(), Some(recovered));

            fixture
                .service
                .refresh_stage_discovery_with(&fixture.project_id, || {
                    scans.set(scans.get() + 1);
                    std::future::ready(Vec::new())
                })
                .await
                .unwrap();
            assert_eq!(scans.get(), 1, "healthy stages should not trigger probes");
        }
    }
}

#[tokio::test]
async fn stage_dispatch_keeps_preflight_blockers_when_the_recheck_still_requires_login() {
    let fixture = stage_fixture();
    for (_, _, role) in hvc_contracts::STAGES {
        configure_role(
            &fixture,
            role,
            AgentExecutionMode::CliProcess,
            CliBackend::Claude,
        );
    }
    let still_unavailable = vec![availability(
        CliBackend::Claude,
        CliAvailabilityState::AuthRequired,
        "not-signed-in",
    )];
    store_cli_discovery(&fixture.service.discovery_cache, still_unavailable.clone());
    let scans = Cell::new(0);
    fixture
        .service
        .refresh_stage_discovery_with(&fixture.project_id, || {
            scans.set(scans.get() + 1);
            std::future::ready(still_unavailable.clone())
        })
        .await
        .unwrap();
    assert_eq!(scans.get(), 1);
    let (items, fresh) = fixture.service.cached_discovery().unwrap();
    assert!(fresh);
    assert_eq!(items, still_unavailable);
    // The real core preflight assertion is retained in terminal_host_integration.
}

#[tokio::test]
async fn stage_discovery_skips_missing_api_custom_and_non_stage_configurations() {
    let fixture = stage_fixture();
    configure_role(
        &fixture,
        AgentRole::IntakeRouter,
        AgentExecutionMode::CliProcess,
        CliBackend::Custom,
    );
    configure_role(
        &fixture,
        AgentRole::FeatureAgent,
        AgentExecutionMode::CliProcess,
        CliBackend::Codex,
    );
    fixture
        .service
        .repository
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2",
            params![fixture.project_id, AgentRole::TaskSpecifier.as_str()],
        )
        .unwrap();
    let cached = vec![availability(
        CliBackend::Codex,
        CliAvailabilityState::AuthRequired,
        "unrelated-to-stage-cli",
    )];
    store_cli_discovery(&fixture.service.discovery_cache, cached.clone());
    let scans = Cell::new(0);
    fixture
        .service
        .refresh_stage_discovery_with(&fixture.project_id, || {
            scans.set(scans.get() + 1);
            std::future::ready(Vec::new())
        })
        .await
        .unwrap();
    assert_eq!(scans.get(), 0);
    assert_eq!(fixture.service.cache_if_fresh(), Some(cached));
}

#[tokio::test]
async fn healthy_stage_discovery_stays_fast_with_a_stale_snapshot_or_unhealthy_other_cli() {
    let fixture = stage_fixture();
    configure_role(
        &fixture,
        AgentRole::IntakeRouter,
        AgentExecutionMode::CliProcess,
        CliBackend::Claude,
    );
    let cached = vec![
        availability(
            CliBackend::Claude,
            CliAvailabilityState::Available,
            "healthy",
        ),
        availability(
            CliBackend::Codex,
            CliAvailabilityState::AuthRequired,
            "other",
        ),
    ];
    store_cli_discovery(&fixture.service.discovery_cache, cached.clone());
    fixture
        .service
        .discovery_cache
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .discovered_at = Instant::now()
        .checked_sub(CLI_DISCOVERY_CACHE_TTL + Duration::from_secs(1))
        .unwrap();
    let scans = Cell::new(0);
    fixture
        .service
        .refresh_stage_discovery_with(&fixture.project_id, || {
            scans.set(scans.get() + 1);
            std::future::ready(Vec::new())
        })
        .await
        .unwrap();
    assert_eq!(scans.get(), 0);
    assert_eq!(fixture.service.cached_discovery(), Some((cached, false)));
}

#[tokio::test]
async fn stage_dispatch_discovers_a_selected_backend_missing_from_the_snapshot() {
    let fixture = stage_fixture();
    configure_role(
        &fixture,
        AgentRole::IntakeRouter,
        AgentExecutionMode::CliProcess,
        CliBackend::Opencode,
    );
    let expected = vec![availability(
        CliBackend::Opencode,
        CliAvailabilityState::Available,
        "new-installation",
    )];
    for initial in [
        None,
        Some(vec![availability(
            CliBackend::Codex,
            CliAvailabilityState::Available,
            "another-cli",
        )]),
    ] {
        *fixture.service.discovery_cache.lock().unwrap() = initial.map(|items| CliDiscoveryCache {
            discovered_at: Instant::now(),
            items,
        });
        let scans = Cell::new(0);
        fixture
            .service
            .refresh_stage_discovery_with(&fixture.project_id, || {
                scans.set(scans.get() + 1);
                std::future::ready(expected.clone())
            })
            .await
            .unwrap();
        assert_eq!(scans.get(), 1);
        assert_eq!(fixture.service.cache_if_fresh(), Some(expected.clone()));
    }
}
