//! Meter behavior with scripted collectors: source order, cache, backoff, kill switch,
//! disclosure, persistence and pre-flight.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::future::BoxFuture;

use super::*;
use crate::persistence_service::PersistenceService;
use crate::terminals::{CliAvailability, CliAvailabilityState, CliBackend, test_availability};

type Script = VecDeque<Result<QuotaReading, SourceError>>;

#[derive(Default)]
struct Scripted {
    app_server: Mutex<Script>,
    rollout: Mutex<Script>,
    endpoint: Mutex<HashMap<CliBackend, Script>>,
    calls: Mutex<Vec<QuotaSource>>,
}

impl Scripted {
    fn next(script: &Mutex<Script>) -> Result<QuotaReading, SourceError> {
        script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(UnavailableReason::SourceFailed.into()))
    }

    fn calls(&self) -> Vec<QuotaSource> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl QuotaCollectors for Scripted {
    fn codex_app_server(
        &self,
        _executable: PathBuf,
    ) -> BoxFuture<'static, Result<QuotaReading, SourceError>> {
        self.calls.lock().unwrap().push(QuotaSource::CodexAppServer);
        let result = Self::next(&self.app_server);
        Box::pin(async move { result })
    }

    fn codex_rollout(&self) -> Result<QuotaReading, SourceError> {
        self.calls.lock().unwrap().push(QuotaSource::CodexRollout);
        Self::next(&self.rollout)
    }

    fn credential_file(&self, backend: CliBackend) -> Option<PathBuf> {
        Some(PathBuf::from(format!(
            "D:/fixture/{}/credentials.json",
            backend.as_str()
        )))
    }

    fn endpoint(
        &self,
        backend: CliBackend,
    ) -> BoxFuture<'static, Result<QuotaReading, SourceError>> {
        self.calls.lock().unwrap().push(match backend {
            CliBackend::Codex => QuotaSource::CodexEndpoint,
            _ => QuotaSource::ClaudeEndpoint,
        });
        let result = self
            .endpoint
            .lock()
            .unwrap()
            .get_mut(&backend)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Err(UnavailableReason::SourceFailed.into()));
        Box::pin(async move { result })
    }
}

fn reading(used: f64, hours_left: i64) -> QuotaReading {
    QuotaReading {
        plan: Some("plus".into()),
        windows: vec![
            QuotaWindow::new(
                None,
                used,
                Some(300),
                Some(Utc::now() + chrono::Duration::hours(hours_left)),
            )
            .unwrap(),
            QuotaWindow::new(
                None,
                10.0,
                Some(10_080),
                Some(Utc::now() + chrono::Duration::days(3)),
            )
            .unwrap(),
        ],
        observed_at: None,
    }
}

fn discovery() -> Vec<CliAvailability> {
    METERED_BACKENDS
        .into_iter()
        .map(|backend| test_availability(backend, CliAvailabilityState::Available))
        .collect()
}

fn database() -> (tempfile::TempDir, PathBuf) {
    let data = tempfile::tempdir().unwrap();
    let persistence = PersistenceService::initialize(data.path()).unwrap();
    let path = persistence.database_path().to_path_buf();
    (data, path)
}

fn meter(collectors: Arc<Scripted>, killed: bool) -> (QuotaMeter, tempfile::TempDir) {
    let (data, path) = database();
    let meter = QuotaMeter::with_collectors(path, collectors, killed);
    meter.set_foreground(true);
    (meter, data)
}

fn script(results: impl IntoIterator<Item = Result<QuotaReading, SourceError>>) -> Mutex<Script> {
    Mutex::new(results.into_iter().collect())
}

fn quota(overview: &QuotaOverview, backend: CliBackend) -> &BackendQuota {
    overview.backend(backend).unwrap()
}

#[tokio::test]
async fn sources_run_in_priority_order_and_the_cache_spares_repeat_reads() {
    let collectors = Arc::new(Scripted {
        app_server: script([Ok(reading(40.0, 2)), Ok(reading(41.0, 2))]),
        endpoint: Mutex::new(HashMap::from([(
            CliBackend::Claude,
            [Ok(reading(12.0, 3))].into_iter().collect(),
        )])),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors.clone(), false);
    let overview = meter.refresh(&discovery(), Utc::now(), false).await;
    assert_eq!(
        collectors.calls(),
        [QuotaSource::CodexAppServer, QuotaSource::ClaudeEndpoint]
    );
    let codex = quota(&overview, CliBackend::Codex);
    assert_eq!(codex.state, QuotaState::Fresh);
    assert_eq!(codex.tightest(Utc::now()).unwrap().used_percent, 40.0);
    assert!(codex.sources[0].active && !codex.sources[2].active);
    // Antigravity reported nothing through its probe.
    assert_eq!(
        quota(&overview, CliBackend::Antigravity).state,
        QuotaState::Unavailable(UnavailableReason::NoChannel)
    );

    // Inside the cache nothing is read again; a forced refresh rereads local sources only:
    // the credential source keeps its 55 s cache (G6).
    meter.refresh(&discovery(), Utc::now(), false).await;
    assert!(collectors.calls().is_empty());
    let overview = meter.refresh(&discovery(), Utc::now(), true).await;
    assert_eq!(collectors.calls(), [QuotaSource::CodexAppServer]);
    assert_eq!(
        quota(&overview, CliBackend::Codex)
            .tightest(Utc::now())
            .unwrap()
            .used_percent,
        41.0
    );
}

#[tokio::test]
async fn failing_sources_fall_through_and_a_stale_record_does_not_stop_the_chain() {
    let mut old = reading(70.0, 2);
    old.observed_at = Some(Utc::now() - chrono::Duration::hours(2));
    let collectors = Arc::new(Scripted {
        app_server: script([Err(UnavailableReason::SourceFailed.into())]),
        rollout: script([Ok(old)]),
        endpoint: Mutex::new(HashMap::from([(
            CliBackend::Codex,
            [Ok(reading(72.0, 2))].into_iter().collect(),
        )])),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors.clone(), false);
    let overview = meter.refresh(&discovery(), Utc::now(), false).await;
    assert_eq!(
        collectors.calls(),
        [
            QuotaSource::CodexAppServer,
            QuotaSource::CodexRollout,
            QuotaSource::CodexEndpoint,
            QuotaSource::ClaudeEndpoint,
        ]
    );
    let codex = quota(&overview, CliBackend::Codex);
    assert_eq!(
        codex.snapshot.as_ref().unwrap().source,
        QuotaSource::CodexEndpoint
    );
    assert_eq!(
        codex.sources[0].last_error,
        Some(UnavailableReason::SourceFailed)
    );
}

#[test]
fn quota_backoff_doubles_to_the_cap_and_honors_429() {
    let start = Instant::now();
    let mut backoff = Backoff::default();
    let waits = (0..6)
        .map(|_| backoff.record_failure(start, UnavailableReason::SourceFailed.into()))
        .map(|wait| wait.as_secs())
        .collect::<Vec<_>>();
    assert_eq!(waits, [110, 220, 440, 880, 900, 900]);
    assert!(!backoff.ready(start + Duration::from_secs(899)));
    assert!(backoff.ready(start + Duration::from_secs(900)));
    backoff.record_success();
    assert!(backoff.ready(start));

    // A Retry-After longer than the backoff is honored as given.
    let throttled = SourceError {
        reason: UnavailableReason::RateLimited,
        retry_after: Some(Duration::from_secs(2_000)),
        throttled: true,
    };
    assert_eq!(backoff.record_failure(start, throttled).as_secs(), 2_000);
    assert!(!backoff.ready(start + Duration::from_secs(1_999)));
    // A 429 without a hint follows the doubling; a refusal without one waits the cap.
    let mut backoff = Backoff::default();
    let no_hint = SourceError {
        retry_after: None,
        ..throttled
    };
    assert_eq!(backoff.record_failure(start, no_hint).as_secs(), 110);
    let refused = SourceError {
        reason: UnavailableReason::Refused,
        retry_after: None,
        throttled: true,
    };
    assert_eq!(
        Backoff::default().record_failure(start, refused),
        BACKOFF_CAP
    );
}

#[tokio::test]
async fn provider_throttling_is_honored_even_by_a_forced_refresh() {
    let collectors = Arc::new(Scripted {
        endpoint: Mutex::new(HashMap::from([(
            CliBackend::Claude,
            [Err(SourceError {
                reason: UnavailableReason::RateLimited,
                retry_after: Some(Duration::from_secs(600)),
                throttled: true,
            })]
            .into_iter()
            .collect(),
        )])),
        app_server: script([Ok(reading(1.0, 2)), Ok(reading(1.0, 2))]),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors.clone(), false);
    let overview = meter.refresh(&discovery(), Utc::now(), false).await;
    assert_eq!(
        quota(&overview, CliBackend::Claude).state,
        QuotaState::Unavailable(UnavailableReason::RateLimited)
    );
    collectors.calls();
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert_eq!(collectors.calls(), [QuotaSource::CodexAppServer]);
}

#[tokio::test]
async fn quota_kill_switch_disables_credential_collectors_only() {
    assert!(flag_enabled(" ON "));
    assert!(flag_enabled("1") && flag_enabled("true") && flag_enabled("Yes"));
    assert!(!flag_enabled("0") && !flag_enabled("") && !flag_enabled("off"));

    let collectors = Arc::new(Scripted {
        app_server: script([Err(UnavailableReason::SourceFailed.into())]),
        rollout: script([Ok(reading(30.0, 2))]),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors.clone(), true);
    let overview = meter.refresh(&discovery(), Utc::now(), false).await;
    // Local sources still run; neither endpoint is ever called.
    assert_eq!(
        collectors.calls(),
        [QuotaSource::CodexAppServer, QuotaSource::CodexRollout]
    );
    assert_eq!(quota(&overview, CliBackend::Codex).state, QuotaState::Fresh);
    let claude = quota(&overview, CliBackend::Claude);
    assert_eq!(
        claude.state,
        QuotaState::Unavailable(UnavailableReason::Disabled)
    );
    assert!(!claude.sources[0].enabled);
    assert!(claude.sources[1].enabled, "run events still count");
    assert!(claude.disclosure.is_none());
    assert!(overview.credential_collectors_off.is_some());
    // Run events still fill the Claude bar.
    assert!(meter.observe_claude_frame(fixture("claude/rate-limit-event.jsonl").trim().as_bytes()));
}

#[tokio::test]
async fn credential_collectors_wait_for_the_foreground() {
    let collectors = Arc::new(Scripted {
        endpoint: Mutex::new(HashMap::from([(
            CliBackend::Claude,
            [Ok(reading(5.0, 2))].into_iter().collect(),
        )])),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors.clone(), false);
    meter.set_foreground(false);
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(!collectors.calls().contains(&QuotaSource::ClaudeEndpoint));
    meter.set_foreground(true);
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(collectors.calls().contains(&QuotaSource::ClaudeEndpoint));
}

#[tokio::test]
async fn quota_first_use_disclosure_names_the_file_and_host_and_can_turn_it_off() {
    let collectors = Arc::new(Scripted {
        endpoint: Mutex::new(HashMap::from([(
            CliBackend::Claude,
            [Ok(reading(5.0, 2))].into_iter().collect(),
        )])),
        ..Scripted::default()
    });
    let (data, path) = database();
    let meter = QuotaMeter::with_collectors(path.clone(), collectors.clone(), false);
    // Nothing to disclose before the collector has run.
    assert!(
        quota(&meter.overview(), CliBackend::Claude)
            .disclosure
            .is_none()
    );
    meter.set_foreground(true);
    let overview = meter.refresh(&discovery(), Utc::now(), false).await;
    let disclosure = quota(&overview, CliBackend::Claude)
        .disclosure
        .clone()
        .unwrap();
    assert_eq!(disclosure.file, "D:/fixture/claude/credentials.json");
    assert_eq!(disclosure.host, "api.anthropic.com");
    meter.acknowledge_disclosure(CliBackend::Claude).unwrap();
    assert!(
        quota(&meter.overview(), CliBackend::Claude)
            .disclosure
            .is_none()
    );

    // "Turn off" persists across restarts and stops the collector.
    meter
        .set_endpoint_switch(CliBackend::Claude, false)
        .unwrap();
    let restarted = QuotaMeter::with_collectors(path, collectors.clone(), false);
    restarted.set_foreground(true);
    collectors.calls();
    let overview = restarted.refresh(&discovery(), Utc::now(), true).await;
    assert!(!collectors.calls().contains(&QuotaSource::ClaudeEndpoint));
    let claude = quota(&overview, CliBackend::Claude);
    assert_eq!(claude.endpoint_switch, Some(false));
    assert!(!claude.sources[0].enabled);
    assert!(claude.disclosure.is_none());
    drop(data);
}

#[tokio::test]
async fn signed_out_or_missing_clis_show_their_state_and_are_not_read() {
    let collectors = Arc::new(Scripted {
        app_server: script([Ok(reading(10.0, 2))]),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors.clone(), false);
    meter.refresh(&discovery(), Utc::now(), false).await;
    collectors.calls();
    let signed_out = vec![
        test_availability(CliBackend::Codex, CliAvailabilityState::AuthRequired),
        test_availability(CliBackend::Claude, CliAvailabilityState::Missing),
        test_availability(
            CliBackend::Antigravity,
            CliAvailabilityState::UnsupportedVersion,
        ),
    ];
    let overview = meter.refresh(&signed_out, Utc::now(), true).await;
    assert!(collectors.calls().is_empty());
    let states = METERED_BACKENDS.map(|backend| quota(&overview, backend).state);
    assert_eq!(
        states,
        [
            QuotaState::Unavailable(UnavailableReason::NotSignedIn),
            QuotaState::Unavailable(UnavailableReason::CliMissing),
            QuotaState::Unavailable(UnavailableReason::VersionUnsupported),
        ]
    );
    assert!(quota(&overview, CliBackend::Codex).snapshot.is_none());
}

#[tokio::test]
async fn the_last_snapshot_survives_a_restart_as_stale() {
    let collectors = Arc::new(Scripted {
        app_server: script([Ok(reading(55.0, 2))]),
        ..Scripted::default()
    });
    let (data, path) = database();
    let meter = QuotaMeter::with_collectors(path.clone(), collectors.clone(), false);
    meter.refresh(&discovery(), Utc::now(), false).await;
    drop(meter);
    let restarted = QuotaMeter::with_collectors(path, collectors, false);
    let codex = restarted.overview();
    let codex = quota(&codex, CliBackend::Codex);
    assert_eq!(codex.state, QuotaState::Stale);
    assert_eq!(codex.tightest(Utc::now()).unwrap().used_percent, 55.0);
    // Stale data never drives the pre-flight.
    assert!(restarted.preflight(CliBackend::Codex, None).is_none());
    drop(data);
}

#[tokio::test]
async fn preflight_warns_only_on_fresh_low_windows() {
    let collectors = Arc::new(Scripted {
        app_server: script([Ok(reading(92.0, 2)), Ok(reading(50.0, 2))]),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors, false);
    meter.refresh(&discovery(), Utc::now(), false).await;
    let finding = meter.preflight(CliBackend::Codex, None).unwrap();
    assert_eq!(finding.window.used_percent, 92.0);
    assert!(finding.message().starts_with("Codex 5h window is 92% used"));
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(meter.preflight(CliBackend::Codex, None).is_none());
    assert!(meter.preflight(CliBackend::Antigravity, None).is_none());
}

#[tokio::test]
async fn claude_run_events_fill_the_bar_and_are_kept_in_history() {
    let collectors = Arc::new(Scripted::default());
    let (meter, _data) = meter(collectors, true);
    meter.refresh(&discovery(), Utc::now(), false).await;
    assert!(meter.observe_claude_frame(fixture("claude/rate-limit-event.jsonl").trim().as_bytes()));
    assert!(!meter.observe_claude_frame(br#"{"type":"assistant"}"#));
    let overview = meter.overview();
    let claude = quota(&overview, CliBackend::Claude);
    let snapshot = claude.snapshot.as_ref().unwrap();
    assert_eq!(snapshot.source, QuotaSource::ClaudeStreamEvent);
    assert_eq!(snapshot.windows.len(), 2);
    assert!(
        meter
            .store()
            .latest(CliBackend::Claude)
            .unwrap()
            .is_some_and(|stored| stored.source == QuotaSource::ClaudeStreamEvent)
    );
}

#[tokio::test]
async fn run_boundaries_attribute_the_change_and_flag_overlaps() {
    let collectors = Arc::new(Scripted {
        app_server: script([
            Ok(reading(10.0, 2)),
            Ok(reading(10.0, 2)),
            Ok(reading(25.0, 2)),
            Ok(reading(31.0, 2)),
        ]),
        ..Scripted::default()
    });
    let (meter, _data) = meter(collectors, true);
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(meter.record_run_boundary(
        CliBackend::Codex,
        "run-a",
        Some("pulse"),
        SampleKind::RunStart
    ));
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(meter.record_run_boundary(CliBackend::Codex, "run-b", None, SampleKind::RunStart));
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(meter.record_run_boundary(
        CliBackend::Codex,
        "run-a",
        Some("pulse"),
        SampleKind::RunEnd
    ));
    meter.refresh(&discovery(), Utc::now(), true).await;
    assert!(meter.record_run_boundary(CliBackend::Codex, "run-b", None, SampleKind::RunEnd));
    let deltas = meter.run_deltas("run-a");
    let five_hour = deltas
        .iter()
        .find(|delta| delta.window_minutes == Some(300))
        .unwrap();
    assert_eq!(five_hour.delta(), 15.0);
    assert!(five_hour.approximate, "run-b overlapped run-a");
    // No data, or data older than two minutes, writes nothing rather than a guess.
    assert!(!meter.record_run_boundary(CliBackend::Claude, "run-c", None, SampleKind::RunStart));
    assert!(meter.run_deltas("run-c").is_empty());
}

#[test]
fn quota_retention_prunes_after_30_days_and_keeps_run_boundary_samples() {
    let (_data, path) = database();
    let store = QuotaStore::new(path);
    let now = Utc::now();
    let at = |days: i64, minutes: i64| {
        now - chrono::Duration::days(days) - chrono::Duration::minutes(minutes)
    };
    let snapshot = |observed_at| {
        QuotaSnapshot::new(
            CliBackend::Codex,
            None,
            vec![QuotaWindow::new(None, 20.0, Some(300), None).unwrap()],
            QuotaSource::CodexAppServer,
            observed_at,
        )
    };
    assert_eq!(
        store
            .record(&snapshot(at(31, 0)), SampleKind::Sample, None)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .record(
                &snapshot(at(31, 0)),
                SampleKind::RunStart,
                Some(("old-run", None))
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .record(&snapshot(at(29, 0)), SampleKind::Sample, None)
            .unwrap(),
        1
    );
    // A second periodic sample inside five minutes is skipped; a run boundary never is.
    assert_eq!(
        store
            .record(&snapshot(at(29, -3)), SampleKind::Sample, None)
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .record(
                &snapshot(at(29, -3)),
                SampleKind::RunEnd,
                Some(("run", Some("pulse")))
            )
            .unwrap(),
        1
    );
    assert_eq!(store.prune(now).unwrap(), 2);
    let connection = rusqlite::Connection::open(store_path(&store)).unwrap();
    let kinds: Vec<String> = connection
        .prepare("SELECT kind FROM quota_snapshots ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(kinds, ["sample", "run_end"]);
}

fn store_path(store: &QuotaStore) -> PathBuf {
    store.db_path().to_path_buf()
}

#[cfg(feature = "quota-credential-collectors")]
mod credential_flow {
    use super::*;
    use crate::quota::http::QuotaHttp;
    use crate::quota::http::test_server::{reply, serve};

    const TOKEN: &str =
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiJxdW90YS1maXh0dXJlIn0.c2lnbmF0dXJlLWZpeHR1cmU";
    const ACCOUNT: &str = "acct-0000-fixture-identifier";

    async fn live_meter(replies: Vec<String>) -> (QuotaMeter, tempfile::TempDir, PathBuf) {
        let server = serve(replies).await;
        let (data, path) = database();
        let claude = data.path().join(".credentials.json");
        let codex = data.path().join("auth.json");
        std::fs::write(
            &claude,
            format!(r#"{{"claudeAiOauth":{{"accessToken":"{TOKEN}","refreshToken":"refresh-{TOKEN}"}}}}"#),
        )
        .unwrap();
        std::fs::write(
            &codex,
            format!(r#"{{"tokens":{{"access_token":"{TOKEN}","refresh_token":"r","account_id":"{ACCOUNT}"}}}}"#),
        )
        .unwrap();
        let collectors = LiveCollectors {
            http: Arc::new(QuotaHttp::for_test(server.origin.clone())),
            codex_sessions: None,
            claude_file: Some(claude),
            codex_file: Some(codex),
        };
        let meter = QuotaMeter::with_collectors(path.clone(), Arc::new(collectors), false);
        meter.set_foreground(true);
        (meter, data, path)
    }

    fn database_text(path: &std::path::Path) -> String {
        let connection = rusqlite::Connection::open(path).unwrap();
        let mut text = String::new();
        for table in ["quota_snapshots", "runtime_settings"] {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table}"))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for column in 0..columns {
                    let value: rusqlite::types::Value = row.get(column).unwrap();
                    text.push_str(&format!("{value:?}|"));
                }
                text.push('\n');
            }
        }
        text
    }

    fn assert_clean(text: &str) {
        for secret in [
            TOKEN,
            "eyJzdWIi",
            ACCOUNT,
            "dev@example.test",
            "user-fixture",
        ] {
            assert!(!text.contains(secret), "{secret} leaked into: {text}");
        }
    }

    /// The Codex app-server fixture path does not exist, so Codex reaches its endpoint.
    fn codex_endpoint_discovery() -> Vec<CliAvailability> {
        discovery()
    }

    #[tokio::test]
    async fn quota_snapshots_store_no_secrets_or_account_identifiers() {
        let codex_body = fixture("codex/usage-endpoint.json")
            .replace("00000000-0000-4000-8000-000000000000", ACCOUNT)
            .replace("\"promo\":null", &format!("\"promo\":\"{TOKEN}\""));
        let (meter, _data, path) = live_meter(vec![
            reply("200 OK", &[], &codex_body),
            reply("200 OK", &[], &fixture("claude/usage-endpoint.json")),
        ])
        .await;
        let overview = meter
            .refresh(&codex_endpoint_discovery(), Utc::now(), false)
            .await;
        assert_eq!(
            quota(&overview, CliBackend::Codex)
                .snapshot
                .as_ref()
                .unwrap()
                .source,
            QuotaSource::CodexEndpoint
        );
        assert_eq!(
            quota(&overview, CliBackend::Claude)
                .snapshot
                .as_ref()
                .unwrap()
                .source,
            QuotaSource::ClaudeEndpoint
        );
        let stored = database_text(&path);
        assert!(stored.contains("codex_endpoint") && stored.contains("claude_endpoint"));
        assert_clean(&stored);
        assert_clean(&format!("{overview:?}"));
        let columns: Vec<String> = rusqlite::Connection::open(&path)
            .unwrap()
            .prepare("SELECT name FROM pragma_table_info('quota_snapshots')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "backend",
                "scope",
                "window_minutes",
                "used_percent",
                "resets_at",
                "source",
                "plan",
                "observed_at",
                "kind",
                "run_id",
                "pulsation_id"
            ]
        );
    }

    #[tokio::test]
    async fn quota_token_never_reaches_logs_events_database_or_errors() {
        // Hostile replies echo the token back; none of it may surface anywhere.
        let echo = format!(r#"{{"error":"invalid token {TOKEN}","account":"{ACCOUNT}"}}"#);
        let (meter, _data, path) = live_meter(vec![
            reply("401 Unauthorized", &[], &echo),
            reply("500 Internal Server Error", &[], &echo),
        ])
        .await;
        let overview = meter
            .refresh(&codex_endpoint_discovery(), Utc::now(), false)
            .await;
        assert_eq!(
            quota(&overview, CliBackend::Codex).state,
            QuotaState::Unavailable(UnavailableReason::SourceFailed)
        );
        assert_eq!(
            quota(&overview, CliBackend::Codex).sources[2].last_error,
            Some(UnavailableReason::SignInExpired)
        );
        assert_eq!(
            quota(&overview, CliBackend::Claude).state,
            QuotaState::Unavailable(UnavailableReason::SourceFailed)
        );
        assert_clean(&format!("{overview:?}"));
        for backend in METERED_BACKENDS {
            let quota = quota(&overview, backend);
            for status in &quota.sources {
                if let Some(error) = status.last_error {
                    assert_clean(error.message());
                }
            }
            if let QuotaState::Unavailable(reason) = quota.state {
                assert_clean(reason.message());
            }
        }
        assert_clean(&database_text(&path));
        // The meter emits no events and writes no logs: its only outputs are the overview and
        // the database, both checked above.
    }

    #[test]
    fn quota_token_is_absent_from_cli_environment_and_argv() {
        // CLI launches build their environment from an allow-list and their argv from the
        // adapter; neither has a channel for a quota token. A token planted in this process's
        // own environment variables would still be filtered by the allow-list.
        for name in crate::terminals::SAFE_ENVIRONMENT {
            let upper = name.to_ascii_uppercase();
            assert!(
                !upper.contains("TOKEN")
                    && !upper.contains("CREDENTIAL")
                    && !upper.contains("OAUTH"),
                "{name} would forward a credential to a CLI"
            );
        }
    }
}
