//! The normalized plan-quota model (ADR-028): every source reports "percent used" per window,
//! whatever the CLI itself reports ("remaining" is converted). The UI only renders values the
//! core validated here.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::terminals::CliBackend;

/// Upper bound on windows kept per snapshot; a reply with more is truncated.
pub const MAX_WINDOWS: usize = 16;
const MAX_LABEL_CHARS: usize = 48;
/// A snapshot older than this is shown as stale with its age.
pub const STALE_AFTER: Duration = Duration::minutes(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    CodexAppServer,
    CodexRollout,
    CodexEndpoint,
    ClaudeEndpoint,
    ClaudeStreamEvent,
    AntigravityUsage,
}

impl QuotaSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CodexAppServer => "codex_app_server",
            Self::CodexRollout => "codex_rollout",
            Self::CodexEndpoint => "codex_endpoint",
            Self::ClaudeEndpoint => "claude_endpoint",
            Self::ClaudeStreamEvent => "claude_stream_event",
            Self::AntigravityUsage => "antigravity_usage",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::CodexAppServer,
            Self::CodexRollout,
            Self::CodexEndpoint,
            Self::ClaudeEndpoint,
            Self::ClaudeStreamEvent,
            Self::AntigravityUsage,
        ]
        .into_iter()
        .find(|source| source.as_str() == value)
    }

    /// How Settings and the popover name the source.
    pub const fn label(self) -> &'static str {
        match self {
            Self::CodexAppServer => "Codex app-server",
            Self::CodexRollout => "Codex session logs",
            Self::CodexEndpoint => "Codex sign-in Â· chatgpt.com",
            Self::ClaudeEndpoint => "Claude Code sign-in Â· api.anthropic.com",
            Self::ClaudeStreamEvent => "Claude run events",
            Self::AntigravityUsage => "agy /usage",
        }
    }

    /// The two collectors that read the CLI's own access token (ADR-028 Â§3).
    pub const fn reads_credential(self) -> bool {
        matches!(self, Self::CodexEndpoint | Self::ClaudeEndpoint)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    NotSignedIn,
    SignInExpired,
    CliMissing,
    VersionUnsupported,
    CredentialStoreNotFile,
    RateLimited,
    /// The provider refused the request (`403`), e.g. it rejects this client.
    Refused,
    UnexpectedResponse,
    /// Turned off by the kill switch, the Settings switch or a build without the feature.
    Disabled,
    /// No source for this CLI produced data yet (e.g. Claude before its first run event).
    NoChannel,
    /// The source could not be reached (process failed, timeout, network).
    SourceFailed,
}

impl UnavailableReason {
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotSignedIn => "Not signed in. Sign in through the CLI, then refresh.",
            Self::SignInExpired => {
                "The CLI's access token expired. It renews the next time the CLI runs; otherwise sign in again."
            }
            Self::CliMissing => "The CLI is not installed.",
            Self::VersionUnsupported => "This CLI version is not validated.",
            Self::CredentialStoreNotFile => {
                "The CLI keeps its sign-in in the system credential store, which the app does not read."
            }
            Self::RateLimited => "The provider asked to wait before the next usage check.",
            Self::Refused => "The provider refused the usage request from this app.",
            Self::UnexpectedResponse => "The usage reply had an unexpected format.",
            Self::Disabled => "Usage reading is turned off for this CLI.",
            Self::NoChannel => "No usage reported yet.",
            Self::SourceFailed => "Usage could not be read. It is retried automatically.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum QuotaState {
    Fresh,
    Stale,
    Exhausted,
    Unavailable(UnavailableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("plan usage reply failed validation")]
pub struct InvalidQuota;

/// One usage window, e.g. the 5-hour or the weekly allowance of one model group.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    /// The model group the window covers, when the CLI meters groups separately.
    pub scope: Option<String>,
    pub used_percent: f64,
    pub window_minutes: Option<u32>,
    pub resets_at: Option<DateTime<Utc>>,
}

impl QuotaWindow {
    /// Validates a reported window (G8): a number outside 0â€“100, a non-finite value or a
    /// zero-length window rejects the whole reply instead of being clamped or guessed.
    pub fn new(
        scope: Option<&str>,
        used_percent: f64,
        window_minutes: Option<u32>,
        resets_at: Option<DateTime<Utc>>,
    ) -> Result<Self, InvalidQuota> {
        if !used_percent.is_finite()
            || !(0.0..=100.0).contains(&used_percent)
            || window_minutes == Some(0)
        {
            return Err(InvalidQuota);
        }
        Ok(Self {
            scope: scope.map(bounded_label).filter(|scope| !scope.is_empty()),
            used_percent: (used_percent * 10.0).round() / 10.0,
            window_minutes,
            resets_at,
        })
    }

    pub fn from_remaining(
        scope: Option<&str>,
        remaining_percent: f64,
        window_minutes: Option<u32>,
        resets_at: Option<DateTime<Utc>>,
    ) -> Result<Self, InvalidQuota> {
        if !remaining_percent.is_finite() {
            return Err(InvalidQuota);
        }
        Self::new(scope, 100.0 - remaining_percent, window_minutes, resets_at)
    }

    /// The window's name, derived from its length and never from a CLI-provided string.
    pub fn label(&self) -> String {
        window_label(self.window_minutes)
    }

    /// A window whose reset has passed no longer describes the current allowance.
    pub fn is_current(&self, now: DateTime<Utc>) -> bool {
        self.resets_at.is_none_or(|reset| reset > now)
    }

    pub fn is_exhausted(&self) -> bool {
        self.used_percent >= 100.0
    }

    /// When the window started, if both its length and its reset are known.
    pub fn starts_at(&self) -> Option<DateTime<Utc>> {
        Some(self.resets_at? - Duration::minutes(i64::from(self.window_minutes?)))
    }

    /// The share of the window already elapsed (the pace marker), in 0â€“1.
    pub fn elapsed_fraction(&self, now: DateTime<Utc>) -> Option<f64> {
        let start = self.starts_at()?;
        let length = f64::from(self.window_minutes?) * 60.0;
        let elapsed = (now - start).num_seconds() as f64;
        Some((elapsed / length).clamp(0.0, 1.0))
    }

    /// Identity of the window across snapshots: same group and same length.
    pub fn key(&self) -> (Option<&str>, Option<u32>) {
        (self.scope.as_deref(), self.window_minutes)
    }
}

pub fn window_label(minutes: Option<u32>) -> String {
    match minutes {
        Some(10_080) => "Weekly".into(),
        Some(1_440) => "Daily".into(),
        Some(minutes) if minutes % 1_440 == 0 => format!("{} days", minutes / 1_440),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes} min"),
        None => "Limit".into(),
    }
}

/// What one source reported at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaSnapshot {
    pub backend: CliBackend,
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub source: QuotaSource,
    pub observed_at: DateTime<Utc>,
}

impl QuotaSnapshot {
    pub fn new(
        backend: CliBackend,
        plan: Option<&str>,
        mut windows: Vec<QuotaWindow>,
        source: QuotaSource,
        observed_at: DateTime<Utc>,
    ) -> Self {
        windows.truncate(MAX_WINDOWS);
        Self {
            backend,
            plan: plan.map(bounded_label).filter(|plan| !plan.is_empty()),
            windows,
            source,
            observed_at,
        }
    }

    pub fn current_windows(&self, now: DateTime<Utc>) -> impl Iterator<Item = &QuotaWindow> {
        self.windows
            .iter()
            .filter(move |window| window.is_current(now))
    }

    /// The current window with the most usage; ties go to the shorter window.
    pub fn tightest(&self, now: DateTime<Utc>) -> Option<&QuotaWindow> {
        self.current_windows(now).max_by(|left, right| {
            left.used_percent
                .total_cmp(&right.used_percent)
                .then_with(|| {
                    right
                        .window_minutes
                        .unwrap_or(u32::MAX)
                        .cmp(&left.window_minutes.unwrap_or(u32::MAX))
                })
        })
    }

    /// The state this snapshot supports on its own, ignoring source errors.
    pub fn state(&self, now: DateTime<Utc>) -> QuotaState {
        if self.current_windows(now).next().is_none() {
            QuotaState::Unavailable(UnavailableReason::NoChannel)
        } else if self.current_windows(now).any(QuotaWindow::is_exhausted) {
            QuotaState::Exhausted
        } else if now - self.observed_at > STALE_AFTER {
            QuotaState::Stale
        } else {
            QuotaState::Fresh
        }
    }
}

pub(crate) fn bounded_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_LABEL_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value).unwrap().to_utc()
    }

    #[test]
    fn windows_reject_values_outside_the_range_instead_of_clamping() {
        for value in [-0.1, 100.1, f64::NAN, f64::INFINITY] {
            assert_eq!(
                QuotaWindow::new(None, value, Some(300), None),
                Err(InvalidQuota)
            );
        }
        assert_eq!(
            QuotaWindow::new(None, 10.0, Some(0), None),
            Err(InvalidQuota)
        );
        let zero = QuotaWindow::new(None, 0.0, Some(300), None).unwrap();
        assert_eq!(zero.used_percent, 0.0);
        let converted =
            QuotaWindow::from_remaining(Some("Gemini"), 95.26, Some(10_080), None).unwrap();
        assert_eq!(converted.used_percent, 4.7);
        assert_eq!(converted.label(), "Weekly");
    }

    #[test]
    fn labels_come_from_the_window_length() {
        assert_eq!(window_label(Some(300)), "5h");
        assert_eq!(window_label(Some(10_080)), "Weekly");
        assert_eq!(window_label(Some(1_440)), "Daily");
        assert_eq!(window_label(Some(43_200)), "30 days");
        assert_eq!(window_label(Some(45)), "45 min");
        assert_eq!(window_label(None), "Limit");
    }

    #[test]
    fn quota_window_past_its_reset_is_not_current() {
        let now = at("2026-10-21T12:00:00Z");
        let past =
            QuotaWindow::new(None, 100.0, Some(300), Some(at("2026-10-21T12:00:00Z"))).unwrap();
        let future =
            QuotaWindow::new(None, 20.0, Some(10_080), Some(at("2026-10-22T00:00:00Z"))).unwrap();
        assert!(!past.is_current(now));
        assert!(future.is_current(now));
        let snapshot = QuotaSnapshot::new(
            CliBackend::Codex,
            None,
            vec![past.clone(), future],
            QuotaSource::CodexAppServer,
            now,
        );
        // The exhausted 5h window already reset, so it neither counts as tightest nor exhausts.
        assert_eq!(snapshot.tightest(now).unwrap().window_minutes, Some(10_080));
        assert_eq!(snapshot.state(now), QuotaState::Fresh);
        let only_past = QuotaSnapshot::new(
            CliBackend::Codex,
            None,
            vec![past],
            QuotaSource::CodexAppServer,
            now,
        );
        assert_eq!(
            only_past.state(now),
            QuotaState::Unavailable(UnavailableReason::NoChannel)
        );
    }

    #[test]
    fn snapshot_state_reports_staleness_and_exhaustion() {
        let now = at("2026-10-21T12:00:00Z");
        let window = |used| {
            QuotaWindow::new(None, used, Some(300), Some(at("2026-10-21T14:00:00Z"))).unwrap()
        };
        let fresh = QuotaSnapshot::new(
            CliBackend::Claude,
            None,
            vec![window(40.0)],
            QuotaSource::ClaudeEndpoint,
            now,
        );
        assert_eq!(fresh.state(now), QuotaState::Fresh);
        assert_eq!(fresh.state(now + Duration::minutes(11)), QuotaState::Stale);
        let exhausted = QuotaSnapshot::new(
            CliBackend::Claude,
            None,
            vec![window(100.0)],
            QuotaSource::ClaudeEndpoint,
            now,
        );
        assert_eq!(exhausted.state(now), QuotaState::Exhausted);
    }

    #[test]
    fn pace_marker_is_the_elapsed_share_of_the_window() {
        let window =
            QuotaWindow::new(None, 10.0, Some(300), Some(at("2026-10-21T15:00:00Z"))).unwrap();
        assert_eq!(
            window.elapsed_fraction(at("2026-10-21T10:00:00Z")),
            Some(0.0)
        );
        assert_eq!(
            window.elapsed_fraction(at("2026-10-21T12:30:00Z")),
            Some(0.5)
        );
        assert_eq!(
            window.elapsed_fraction(at("2026-10-21T16:00:00Z")),
            Some(1.0)
        );
        assert_eq!(
            QuotaWindow::new(None, 1.0, None, None)
                .unwrap()
                .elapsed_fraction(Utc::now()),
            None
        );
    }
}
