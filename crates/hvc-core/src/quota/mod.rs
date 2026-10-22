//! Plan-quota meter (ADR-028): how much of each subscription CLI's 5-hour, weekly and other
//! windows is used. Official and local sources come first; the credential-reading collectors
//! (Claude and Codex usage endpoints) exist only with the `quota-credential-collectors`
//! feature and obey guardrails G1–G10.

pub mod model;
pub mod pace;
pub(crate) mod sources;
pub mod store;

#[cfg(feature = "quota-credential-collectors")]
pub mod credentials;
#[cfg(feature = "quota-credential-collectors")]
pub(crate) mod endpoints;
#[cfg(feature = "quota-credential-collectors")]
pub mod http;

#[cfg(test)]
mod meter_tests;

/// A synthetic reply from `hvc-test-support/fixtures/quota`.
#[cfg(test)]
pub(crate) fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../hvc-test-support/fixtures/quota")
            .join(name),
    )
    .unwrap()
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;

pub use model::{
    QuotaSnapshot, QuotaSource, QuotaState, QuotaWindow, STALE_AFTER, UnavailableReason,
    window_label,
};
pub use pace::Projection;
pub use store::{QuotaStore, RunQuotaDelta, SampleKind};

use crate::terminals::{CliAvailability, CliAvailabilityState, CliBackend};

/// Fresh data is served from memory for this long (G6).
pub const CACHE_FOR: Duration = Duration::from_secs(55);
/// Failure backoff doubles up to this cap (G6).
pub const BACKOFF_CAP: Duration = Duration::from_secs(15 * 60);
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
/// Process-backed sources start a CLI per read (S1: about 8 s for `codex app-server`), so they
/// are read at most this often, like the Antigravity probe's discovery cache.
const PROCESS_SOURCE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// A run-boundary sample is written only from data observed this close to the boundary.
const BOUNDARY_MAX_AGE: chrono::Duration = chrono::Duration::minutes(2);
/// Pre-flight warns at or above this share of the tightest window.
pub const PREFLIGHT_USED_PERCENT: f64 = 90.0;

pub const METERED_BACKENDS: [CliBackend; 3] = [
    CliBackend::Codex,
    CliBackend::Claude,
    CliBackend::Antigravity,
];

/// Turns off every credential-reading collector and nothing else (G7).
pub const KILL_SWITCH: &str = "HVC_DISABLE_QUOTA_ENDPOINTS";

/// Spike S1 (2026-10-21): the Codex usage endpoint answered `200` to the app's honest
/// `User-Agent` and no `originator` header. Were it `false`, that collector would stay off;
/// impersonating the official client requires an explicit, recorded maintainer acceptance
/// (ADR-028 G9) and is not implemented.
pub const CODEX_ENDPOINT_ACCEPTS_HONEST_IDENTITY: bool = true;

/// `1`, `true`, `yes` or `on` (trimmed, any case), like the other local kill switches.
pub fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| flag_enabled(&value))
}

pub(crate) fn flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Whether this build contains the credential-reading collectors (G10).
pub const fn credential_collectors_compiled() -> bool {
    cfg!(feature = "quota-credential-collectors")
}

/// What one collector returned.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaReading {
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    /// When the data was true; `None` means "now".
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceError {
    pub reason: UnavailableReason,
    /// The provider asked to wait (`429`/`403`); never retried sooner.
    pub retry_after: Option<Duration>,
    pub throttled: bool,
}

impl From<UnavailableReason> for SourceError {
    fn from(reason: UnavailableReason) -> Self {
        Self {
            reason,
            retry_after: None,
            throttled: false,
        }
    }
}

/// The collectors behind the meter; a trait so tests can script every source.
pub(crate) trait QuotaCollectors: Send + Sync {
    fn codex_app_server(
        &self,
        executable: PathBuf,
    ) -> BoxFuture<'static, Result<QuotaReading, SourceError>>;
    fn codex_rollout(&self) -> Result<QuotaReading, SourceError>;
    /// The credential file a credential collector would read, for the disclosure.
    fn credential_file(&self, backend: CliBackend) -> Option<PathBuf>;
    /// The provider usage endpoint with the CLI's own token (feature builds only).
    fn endpoint(
        &self,
        backend: CliBackend,
    ) -> BoxFuture<'static, Result<QuotaReading, SourceError>>;
}

/// The real collectors. Paths are resolved once, from the CLIs' own directory variables.
pub(crate) struct LiveCollectors {
    #[cfg(feature = "quota-credential-collectors")]
    pub(crate) http: Arc<http::QuotaHttp>,
    pub(crate) codex_sessions: Option<PathBuf>,
    pub(crate) claude_file: Option<PathBuf>,
    pub(crate) codex_file: Option<PathBuf>,
}

impl LiveCollectors {
    fn from_environment() -> Self {
        Self {
            #[cfg(feature = "quota-credential-collectors")]
            http: Arc::new(http::QuotaHttp::new()),
            codex_sessions: sources::codex_sessions_root(),
            #[cfg(feature = "quota-credential-collectors")]
            claude_file: credentials::claude_credentials_path(),
            #[cfg(feature = "quota-credential-collectors")]
            codex_file: credentials::codex_auth_path(),
            #[cfg(not(feature = "quota-credential-collectors"))]
            claude_file: None,
            #[cfg(not(feature = "quota-credential-collectors"))]
            codex_file: None,
        }
    }
}

impl QuotaCollectors for LiveCollectors {
    fn codex_app_server(
        &self,
        executable: PathBuf,
    ) -> BoxFuture<'static, Result<QuotaReading, SourceError>> {
        Box::pin(async move {
            crate::terminals::read_codex_app_server_usage(&executable)
                .await
                .map_err(SourceError::from)
        })
    }

    fn codex_rollout(&self) -> Result<QuotaReading, SourceError> {
        let root = self
            .codex_sessions
            .as_deref()
            .ok_or(UnavailableReason::NoChannel)?;
        Ok(sources::latest_codex_rollout(root)?)
    }

    fn credential_file(&self, backend: CliBackend) -> Option<PathBuf> {
        match backend {
            CliBackend::Claude => self.claude_file.clone(),
            CliBackend::Codex => self.codex_file.clone(),
            _ => None,
        }
    }

    fn endpoint(
        &self,
        backend: CliBackend,
    ) -> BoxFuture<'static, Result<QuotaReading, SourceError>> {
        #[cfg(feature = "quota-credential-collectors")]
        {
            let http = self.http.clone();
            let file = self.credential_file(backend);
            Box::pin(
                async move { endpoints::read(&http, backend, file.as_deref(), Utc::now()).await },
            )
        }
        #[cfg(not(feature = "quota-credential-collectors"))]
        {
            let _ = backend;
            Box::pin(async { Err(SourceError::from(UnavailableReason::Disabled)) })
        }
    }
}

/// Retry pacing of one source (G6).
#[derive(Debug, Clone, Default)]
pub(crate) struct Backoff {
    failures: u32,
    not_before: Option<Instant>,
}

impl Backoff {
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|not_before| now >= not_before)
    }

    /// The wait after the `failures`-th consecutive failure: 110 s, 220 s, … capped at 15 min.
    pub(crate) fn interval(failures: u32) -> Duration {
        CACHE_FOR
            .saturating_mul(2u32.saturating_pow(failures.min(16)))
            .min(BACKOFF_CAP)
    }

    pub(crate) fn record_failure(&mut self, now: Instant, error: SourceError) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let mut wait = Self::interval(self.failures);
        if error.throttled {
            wait = wait.max(match error.retry_after {
                Some(retry_after) => retry_after,
                // A refusal without a hint waits the full cap.
                None if error.reason == UnavailableReason::Refused => BACKOFF_CAP,
                None => wait,
            });
        }
        self.not_before = Some(now + wait);
        wait
    }

    pub(crate) fn record_success(&mut self) {
        *self = Self::default();
    }
}

/// The disclosure shown once, the first time a credential collector runs (G7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialDisclosure {
    pub backend: CliBackend,
    pub file: String,
    pub host: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    pub source: QuotaSource,
    /// Compiled in and not turned off (kill switch, Settings switch, G9).
    pub enabled: bool,
    /// The source the shown data came from.
    pub active: bool,
    pub last_error: Option<UnavailableReason>,
}

/// Everything the UI renders for one CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendQuota {
    pub backend: CliBackend,
    pub state: QuotaState,
    /// The last good observation, `None` when the CLI is missing or signed out.
    pub snapshot: Option<QuotaSnapshot>,
    /// Aligned with `snapshot.windows`.
    pub projections: Vec<Option<Projection>>,
    pub sources: Vec<SourceStatus>,
    pub disclosure: Option<CredentialDisclosure>,
    /// Whether the Settings switch for the credential collector is on.
    pub endpoint_switch: Option<bool>,
}

impl BackendQuota {
    /// The current window with the most usage, when data can be shown.
    pub fn tightest(&self, now: DateTime<Utc>) -> Option<&QuotaWindow> {
        self.snapshot.as_ref()?.tightest(now)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaOverview {
    pub backends: Vec<BackendQuota>,
    pub checked_at: DateTime<Utc>,
    /// Kill switch set or feature compiled out: say so in Settings.
    pub credential_collectors_off: Option<&'static str>,
}

impl QuotaOverview {
    pub fn backend(&self, backend: CliBackend) -> Option<&BackendQuota> {
        self.backends.iter().find(|item| item.backend == backend)
    }
}

/// An advisory pre-flight finding: the stage's CLI is low on its tightest window.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaPreflight {
    pub backend: CliBackend,
    pub window: QuotaWindow,
    pub projection: Option<Projection>,
}

impl QuotaPreflight {
    pub fn message(&self) -> String {
        let label = self.window.label();
        let scope = self
            .window
            .scope
            .as_ref()
            .map(|scope| format!(" ({scope})"))
            .unwrap_or_default();
        let reset = self
            .window
            .resets_at
            .map(|reset| format!(", resets {}", reset.format("%Y-%m-%d %H:%M UTC")))
            .unwrap_or_default();
        match self.projection {
            Some(projection) if self.window.used_percent < PREFLIGHT_USED_PERCENT => format!(
                "{} {label} window{scope} is {:.0}% used and, at the current pace, is estimated to run out at {} — before this stage would finish{reset}.",
                self.backend.label(),
                self.window.used_percent,
                projection.exhausts_at.format("%Y-%m-%d %H:%M UTC"),
            ),
            _ => format!(
                "{} {label} window{scope} is {:.0}% used{reset}.",
                self.backend.label(),
                self.window.used_percent,
            ),
        }
    }
}

#[derive(Default)]
struct BackendState {
    snapshot: Option<QuotaSnapshot>,
    collected_at: Option<Instant>,
    cli_reason: Option<UnavailableReason>,
    errors: HashMap<QuotaSource, UnavailableReason>,
    backoff: HashMap<QuotaSource, Backoff>,
    last_success: HashMap<QuotaSource, Instant>,
    credential_used: bool,
    /// Loaded from history at startup and not collected since: shown as stale.
    reloaded: bool,
}

/// How long one successful read of `source` stands before the source is asked again.
const fn min_interval(source: QuotaSource) -> Duration {
    match source {
        QuotaSource::CodexAppServer | QuotaSource::AntigravityUsage => PROCESS_SOURCE_INTERVAL,
        _ => CACHE_FOR,
    }
}

pub struct QuotaMeter {
    store: QuotaStore,
    collectors: Arc<dyn QuotaCollectors>,
    endpoints_killed: bool,
    foreground: AtomicBool,
    state: Mutex<HashMap<CliBackend, BackendState>>,
    last_prune: Mutex<Option<Instant>>,
    refreshing: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for QuotaMeter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("QuotaMeter")
    }
}

fn endpoint_key(backend: CliBackend) -> String {
    format!("quota.endpoint.{}.enabled", backend.as_str())
}

fn disclosure_key(backend: CliBackend) -> String {
    format!("quota.endpoint.{}.disclosure_seen_at", backend.as_str())
}

const fn endpoint_host(backend: CliBackend) -> &'static str {
    match backend {
        CliBackend::Codex => "chatgpt.com",
        _ => "api.anthropic.com",
    }
}

const fn endpoint_source(backend: CliBackend) -> Option<QuotaSource> {
    match backend {
        CliBackend::Codex => Some(QuotaSource::CodexEndpoint),
        CliBackend::Claude => Some(QuotaSource::ClaudeEndpoint),
        _ => None,
    }
}

/// Source priority per CLI (ADR-028 §2; Codex order fixed 2026-10-21).
pub const fn source_chain(backend: CliBackend) -> &'static [QuotaSource] {
    match backend {
        CliBackend::Codex => &[
            QuotaSource::CodexAppServer,
            QuotaSource::CodexRollout,
            QuotaSource::CodexEndpoint,
        ],
        CliBackend::Claude => &[QuotaSource::ClaudeEndpoint, QuotaSource::ClaudeStreamEvent],
        CliBackend::Antigravity => &[QuotaSource::AntigravityUsage],
        _ => &[],
    }
}

fn cli_reason(availability: Option<&CliAvailability>) -> Option<UnavailableReason> {
    match availability.map(|item| item.state) {
        None | Some(CliAvailabilityState::Missing) => Some(UnavailableReason::CliMissing),
        Some(CliAvailabilityState::Available) => None,
        Some(CliAvailabilityState::AuthRequired) => Some(UnavailableReason::NotSignedIn),
        Some(
            CliAvailabilityState::UnsupportedVersion | CliAvailabilityState::CapabilityMissing,
        ) => Some(UnavailableReason::VersionUnsupported),
        Some(CliAvailabilityState::Unsupported) => Some(UnavailableReason::Disabled),
        Some(CliAvailabilityState::Probing | CliAvailabilityState::ProbeFailed) => {
            Some(UnavailableReason::CliCheckFailed)
        }
    }
}

impl QuotaMeter {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self::with_collectors(
            db_path,
            Arc::new(LiveCollectors::from_environment()),
            env_flag(KILL_SWITCH),
        )
    }

    pub(crate) fn with_collectors(
        db_path: impl Into<PathBuf>,
        collectors: Arc<dyn QuotaCollectors>,
        endpoints_killed: bool,
    ) -> Self {
        let store = QuotaStore::new(db_path);
        let mut state = HashMap::new();
        for backend in METERED_BACKENDS {
            let snapshot = store.latest(backend).ok().flatten();
            state.insert(
                backend,
                BackendState {
                    reloaded: snapshot.is_some(),
                    snapshot,
                    ..BackendState::default()
                },
            );
        }
        Self {
            store,
            collectors,
            endpoints_killed,
            foreground: AtomicBool::new(false),
            state: Mutex::new(state),
            last_prune: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn store(&self) -> &QuotaStore {
        &self.store
    }

    /// Credential collectors run only while the app window is active (G6).
    pub fn set_foreground(&self, foreground: bool) {
        self.foreground.store(foreground, Ordering::Relaxed);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CliBackend, BackendState>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The Settings switch of a CLI's credential collector; on unless turned off (G7).
    pub fn endpoint_switch(&self, backend: CliBackend) -> bool {
        self.store
            .setting(&endpoint_key(backend))
            .ok()
            .flatten()
            .is_none_or(|value| value != "false")
    }

    pub fn set_endpoint_switch(&self, backend: CliBackend, enabled: bool) -> rusqlite::Result<()> {
        if endpoint_source(backend).is_none() {
            return Ok(());
        }
        self.store.set_setting(
            &endpoint_key(backend),
            if enabled { "true" } else { "false" },
        )?;
        if !enabled {
            // Turning it off also answers the disclosure.
            self.acknowledge_disclosure(backend)?;
        }
        // The next refresh applies the switch instead of serving the cache.
        let mut state = self.lock();
        if let Some(entry) = state.get_mut(&backend) {
            entry.collected_at = None;
        }
        Ok(())
    }

    pub fn acknowledge_disclosure(&self, backend: CliBackend) -> rusqlite::Result<()> {
        self.store
            .set_setting(&disclosure_key(backend), &Utc::now().to_rfc3339())
    }

    fn source_enabled(&self, backend: CliBackend, source: QuotaSource) -> bool {
        if !source.reads_credential() {
            return true;
        }
        credential_collectors_compiled()
            && !self.endpoints_killed
            && (source != QuotaSource::CodexEndpoint || CODEX_ENDPOINT_ACCEPTS_HONEST_IDENTITY)
            && self.endpoint_switch(backend)
    }

    fn credential_collectors_off(&self) -> Option<&'static str> {
        if !credential_collectors_compiled() {
            Some("This build does not include the sign-in usage readers.")
        } else if self.endpoints_killed {
            Some("Sign-in usage readers are turned off by HVC_DISABLE_QUOTA_ENDPOINTS.")
        } else {
            None
        }
    }

    /// Collects every metered CLI that is signed in, honoring the cache and backoff, then
    /// returns what the UI shows. `force` skips the cache and backoff of local sources only;
    /// credential sources keep their 55 s cache and provider throttling is always honored.
    /// `discovered_at` is when `discovery` was probed; Antigravity's windows date from then.
    pub async fn refresh(
        &self,
        discovery: &[CliAvailability],
        discovered_at: DateTime<Utc>,
        force: bool,
    ) -> QuotaOverview {
        let _single_flight = self.refreshing.lock().await;
        for backend in METERED_BACKENDS {
            let availability = discovery.iter().find(|item| item.backend == backend);
            self.refresh_backend(backend, availability, discovered_at, force)
                .await;
        }
        self.prune_if_due();
        self.overview()
    }

    async fn refresh_backend(
        &self,
        backend: CliBackend,
        availability: Option<&CliAvailability>,
        discovered_at: DateTime<Utc>,
        force: bool,
    ) {
        let reason = cli_reason(availability);
        {
            let mut state = self.lock();
            let entry = state.entry(backend).or_default();
            entry.cli_reason = reason;
            if reason.is_some()
                || (!force
                    && entry
                        .collected_at
                        .is_some_and(|collected| collected.elapsed() < CACHE_FOR))
            {
                return;
            }
        }
        let foreground = self.foreground.load(Ordering::Relaxed);
        for &source in source_chain(backend) {
            if !self.source_enabled(backend, source) {
                self.lock()
                    .entry(backend)
                    .or_default()
                    .errors
                    .insert(source, UnavailableReason::Disabled);
                continue;
            }
            if source == QuotaSource::ClaudeStreamEvent {
                // Passive: filled by `observe_claude_frame` during runs.
                continue;
            }
            if source.reads_credential() && !foreground {
                continue;
            }
            let now = Instant::now();
            let (ready, recent) = {
                let state = self.lock();
                let entry = state.get(&backend);
                (
                    entry
                        .and_then(|entry| entry.backoff.get(&source))
                        .is_none_or(|backoff| backoff.ready(now)),
                    entry
                        .and_then(|entry| entry.last_success.get(&source))
                        .is_some_and(|at| now.duration_since(*at) < min_interval(source)),
                )
            };
            // A credential source keeps its 55 s cache even on a forced refresh (G6).
            if recent && (!force || source.reads_credential()) {
                // This source's last read still stands; lower-priority sources are not needed.
                return;
            }
            if !ready && !(force && !source.reads_credential()) {
                continue;
            }
            let result = match source {
                QuotaSource::CodexAppServer => {
                    match availability.and_then(|item| item.executable_path.clone()) {
                        Some(executable) => self.collectors.codex_app_server(executable).await,
                        None => Err(UnavailableReason::CliMissing.into()),
                    }
                }
                QuotaSource::CodexRollout => self.collectors.codex_rollout(),
                QuotaSource::CodexEndpoint | QuotaSource::ClaudeEndpoint => {
                    self.lock().entry(backend).or_default().credential_used = true;
                    self.collectors.endpoint(backend).await
                }
                QuotaSource::AntigravityUsage => match availability {
                    Some(item) if !item.quota.is_empty() => {
                        sources::antigravity_reading(&item.quota, Some(discovered_at))
                            .map_err(SourceError::from)
                    }
                    _ => Err(UnavailableReason::NoChannel.into()),
                },
                QuotaSource::ClaudeStreamEvent => continue,
            };
            if self.accept(backend, source, result) {
                break;
            }
        }
    }

    /// Applies one collector result. `true` when the data now shown is current enough to stop
    /// walking the chain; a stale passive record is kept but the next source is still tried.
    fn accept(
        &self,
        backend: CliBackend,
        source: QuotaSource,
        result: Result<QuotaReading, SourceError>,
    ) -> bool {
        let now = Instant::now();
        let reading = match result {
            Ok(reading) if !reading.windows.is_empty() => reading,
            Ok(_) => {
                let mut state = self.lock();
                let entry = state.entry(backend).or_default();
                entry.backoff.entry(source).or_default().record_success();
                entry.errors.insert(source, UnavailableReason::NoChannel);
                return false;
            }
            Err(error) => {
                let mut state = self.lock();
                let entry = state.entry(backend).or_default();
                entry
                    .backoff
                    .entry(source)
                    .or_default()
                    .record_failure(now, error);
                entry.errors.insert(source, error.reason);
                return false;
            }
        };
        let snapshot = QuotaSnapshot::new(
            backend,
            reading.plan.as_deref(),
            reading.windows,
            source,
            reading.observed_at.unwrap_or_else(Utc::now),
        );
        let (newer, current) = {
            let mut state = self.lock();
            let entry = state.entry(backend).or_default();
            entry.backoff.entry(source).or_default().record_success();
            entry.errors.remove(&source);
            entry.collected_at = Some(now);
            // A record older than what is already shown does not replace it.
            let newer = entry
                .snapshot
                .as_ref()
                .is_none_or(|shown| snapshot.observed_at >= shown.observed_at);
            if newer {
                entry.snapshot = Some(snapshot.clone());
                entry.reloaded = false;
            }
            let current = entry.snapshot.as_ref().is_some_and(|shown| {
                matches!(
                    shown.state(Utc::now()),
                    QuotaState::Fresh | QuotaState::Exhausted
                )
            });
            if current {
                entry.last_success.insert(source, now);
            }
            (newer, current)
        };
        if newer {
            let _ = self.store.record(&snapshot, SampleKind::Sample, None);
        }
        current
    }

    fn prune_if_due(&self) {
        let mut last = self
            .last_prune
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.is_none_or(|at| at.elapsed() >= PRUNE_EVERY) {
            *last = Some(Instant::now());
            let _ = self.store.prune(Utc::now());
        }
    }

    /// The current view without collecting anything.
    pub fn overview(&self) -> QuotaOverview {
        let now = Utc::now();
        let state = self.lock();
        let mut backends = Vec::new();
        for backend in METERED_BACKENDS {
            let default = BackendState::default();
            let entry = state.get(&backend).unwrap_or(&default);
            let snapshot = entry
                .cli_reason
                .is_none()
                .then(|| entry.snapshot.clone())
                .flatten();
            let quota_state = match (entry.cli_reason, &snapshot) {
                (Some(reason), _) => QuotaState::Unavailable(reason),
                (None, Some(snapshot)) if snapshot.current_windows(now).next().is_some() => {
                    match snapshot.state(now) {
                        QuotaState::Fresh if entry.reloaded => QuotaState::Stale,
                        state => state,
                    }
                }
                (None, _) => QuotaState::Unavailable(
                    source_chain(backend)
                        .iter()
                        .find_map(|source| entry.errors.get(source).copied())
                        .unwrap_or(UnavailableReason::NoChannel),
                ),
            };
            let projections = snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .windows
                        .iter()
                        .map(|window| {
                            let since = window.starts_at()?;
                            let samples = self.store.samples(backend, window, since).ok()?;
                            pace::project(window, &samples, now)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let sources = source_chain(backend)
                .iter()
                .map(|&source| SourceStatus {
                    source,
                    enabled: self.source_enabled(backend, source),
                    active: snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.source == source),
                    last_error: entry.errors.get(&source).copied(),
                })
                .collect();
            let disclosure = (entry.credential_used
                && endpoint_source(backend)
                    .is_some_and(|source| self.source_enabled(backend, source))
                && self
                    .store
                    .setting(&disclosure_key(backend))
                    .ok()
                    .flatten()
                    .is_none())
            .then(|| CredentialDisclosure {
                backend,
                file: self
                    .collectors
                    .credential_file(backend)
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                host: endpoint_host(backend),
            });
            backends.push(BackendQuota {
                backend,
                state: quota_state,
                snapshot,
                projections,
                sources,
                disclosure,
                endpoint_switch: endpoint_source(backend).map(|_| self.endpoint_switch(backend)),
            });
        }
        QuotaOverview {
            backends,
            checked_at: now,
            credential_collectors_off: self.credential_collectors_off(),
        }
    }

    /// Records the windows of `backend` at a run boundary, from data observed within two
    /// minutes; otherwise nothing is written and the run gets no attribution.
    pub fn record_run_boundary(
        &self,
        backend: CliBackend,
        run_id: &str,
        pulsation_id: Option<&str>,
        kind: SampleKind,
    ) -> bool {
        let snapshot = self
            .lock()
            .get(&backend)
            .and_then(|entry| entry.snapshot.clone());
        let Some(snapshot) = snapshot else {
            return false;
        };
        if Utc::now() - snapshot.observed_at > BOUNDARY_MAX_AGE {
            return false;
        }
        if kind == SampleKind::RunEnd && !self.store.has_run_start(run_id).unwrap_or(false) {
            return false;
        }
        self.store
            .record(&snapshot, kind, Some((run_id, pulsation_id)))
            .is_ok_and(|written| written > 0)
    }

    /// Refreshes `backend` through its chain (cache and backoff apply), then records the
    /// boundary. Used by the run lifecycle; never fails the run.
    pub async fn refresh_and_record_boundary(
        &self,
        availability: Option<CliAvailability>,
        discovered_at: DateTime<Utc>,
        backend: CliBackend,
        run_id: String,
        pulsation_id: Option<String>,
        kind: SampleKind,
    ) {
        if !METERED_BACKENDS.contains(&backend) {
            return;
        }
        {
            let _single_flight = self.refreshing.lock().await;
            self.refresh_backend(backend, availability.as_ref(), discovered_at, false)
                .await;
        }
        self.record_run_boundary(backend, &run_id, pulsation_id.as_deref(), kind);
    }

    /// Applies a Claude run's own rate-limit frame (passive source; G8 validation applies).
    pub fn observe_claude_frame(&self, line: &[u8]) -> bool {
        let Some(reading) = sources::claude_rate_limit_frame(line) else {
            return false;
        };
        let source = QuotaSource::ClaudeStreamEvent;
        match reading {
            Ok(reading) => self.accept(CliBackend::Claude, source, Ok(reading)),
            Err(reason) => {
                self.lock()
                    .entry(CliBackend::Claude)
                    .or_default()
                    .errors
                    .insert(source, reason);
                false
            }
        }
    }

    pub fn run_deltas(&self, run_id: &str) -> Vec<RunQuotaDelta> {
        self.store.run_deltas(run_id).unwrap_or_default()
    }

    /// Attribution of many runs at once, keyed by run id (runs without it are omitted).
    pub fn run_deltas_many<'a>(
        &self,
        run_ids: impl IntoIterator<Item = &'a str>,
    ) -> HashMap<String, Vec<RunQuotaDelta>> {
        self.store.run_deltas_many(run_ids).unwrap_or_default()
    }

    /// Advisory only (ADR-028 §6): a finding when the tightest current window of `backend` is
    /// at or above 90% used, or projected to run out within `expected` (the stage's typical
    /// duration). Stale or missing data never produces a finding.
    pub fn preflight(
        &self,
        backend: CliBackend,
        expected: Option<chrono::Duration>,
    ) -> Option<QuotaPreflight> {
        let now = Utc::now();
        let overview = self.overview();
        let quota = overview.backend(backend)?;
        if !matches!(quota.state, QuotaState::Fresh | QuotaState::Exhausted) {
            return None;
        }
        let snapshot = quota.snapshot.as_ref()?;
        let mut worst: Option<QuotaPreflight> = None;
        for (index, window) in snapshot.windows.iter().enumerate() {
            if !window.is_current(now) {
                continue;
            }
            let projection = quota.projections.get(index).copied().flatten();
            let runs_out = projection.is_some_and(|projection| {
                projection.before_reset
                    && expected.is_some_and(|expected| projection.exhausts_at <= now + expected)
            });
            if window.used_percent >= PREFLIGHT_USED_PERCENT || runs_out {
                let finding = QuotaPreflight {
                    backend,
                    window: window.clone(),
                    projection,
                };
                if worst
                    .as_ref()
                    .is_none_or(|current| window.used_percent > current.window.used_percent)
                {
                    worst = Some(finding);
                }
            }
        }
        worst
    }
}
