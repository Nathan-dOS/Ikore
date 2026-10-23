//! Credential-free quota sources: Codex rollout records and Antigravity's `/usage` buckets.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::QuotaReading;
use super::model::{QuotaWindow, UnavailableReason, bounded_label};
use crate::terminals::CliQuotaBucket;

/// Only the newest rollouts are read, and only their tail (S4: 256 KiB of an 11.5 MB rollout
/// took 11 ms and held 7 `rate_limits` records).
const ROLLOUT_FILES: usize = 8;
const ROLLOUT_TAIL_BYTES: u64 = 256 * 1024;
const MAX_ROLLOUT_LINE: usize = 64 * 1024;

/// `<CODEX_HOME or ~/.codex>/sessions`. Unlike model provenance, this honors `CODEX_HOME`.
pub(crate) fn codex_sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
                .map(|home| PathBuf::from(home).join(".codex"))
        })?;
    Some(home.join("sessions"))
}

#[derive(Deserialize)]
struct RolloutLine {
    timestamp: String,
    payload: RolloutPayload,
}

#[derive(Deserialize)]
struct RolloutPayload {
    rate_limits: Option<CodexLimits>,
}

/// The `rate_limits` record Codex writes into its rollouts (S4, Codex 0.162.1).
#[derive(Deserialize)]
pub(crate) struct CodexLimits {
    limit_id: Option<String>,
    limit_name: Option<String>,
    primary: Option<CodexWindow>,
    secondary: Option<CodexWindow>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct CodexWindow {
    used_percent: f64,
    window_minutes: Option<u32>,
    resets_at: Option<i64>,
}

/// One group's scope: the shared `codex` limit is the CLI's own and needs no label.
pub(crate) fn codex_scope(limit_id: Option<&str>, limit_name: Option<&str>) -> Option<String> {
    limit_name
        .or(limit_id.filter(|id| !id.eq_ignore_ascii_case("codex")))
        .map(bounded_label)
        .filter(|scope| !scope.is_empty())
}

pub(crate) fn epoch_seconds(value: i64) -> Result<DateTime<Utc>, UnavailableReason> {
    DateTime::from_timestamp(value, 0).ok_or(UnavailableReason::UnexpectedResponse)
}

impl CodexLimits {
    fn into_reading(self, observed_at: DateTime<Utc>) -> Result<QuotaReading, UnavailableReason> {
        let scope = codex_scope(self.limit_id.as_deref(), self.limit_name.as_deref());
        let mut windows = Vec::new();
        for window in [self.primary, self.secondary].into_iter().flatten() {
            let resets_at = window.resets_at.map(epoch_seconds).transpose()?;
            windows.push(
                QuotaWindow::new(
                    scope.as_deref(),
                    window.used_percent,
                    window.window_minutes,
                    resets_at,
                )
                .map_err(|_| UnavailableReason::UnexpectedResponse)?,
            );
        }
        Ok(QuotaReading {
            plan: self.plan_type,
            windows,
            observed_at: Some(observed_at),
        })
    }
}

/// The most recent `rate_limits` record across the newest rollouts. Passive: it only changes
/// while Codex runs, and its timestamp is the record's own.
pub(crate) fn latest_codex_rollout(root: &Path) -> Result<QuotaReading, UnavailableReason> {
    let mut files = walkdir::WalkDir::new(root)
        .max_depth(4)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.into_path()))
        })
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Err(UnavailableReason::NoChannel);
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.0));
    let mut best: Option<(DateTime<Utc>, CodexLimits)> = None;
    for (_, path) in files.into_iter().take(ROLLOUT_FILES) {
        let Ok(Some((observed_at, limits))) = latest_record_in(&path) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| observed_at > *seen) {
            best = Some((observed_at, limits));
        }
    }
    let (observed_at, limits) = best.ok_or(UnavailableReason::NoChannel)?;
    limits.into_reading(observed_at)
}

fn latest_record_in(path: &Path) -> std::io::Result<Option<(DateTime<Utc>, CodexLimits)>> {
    // std opens with shared read/write access, so a rollout Codex is still writing can be read.
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(ROLLOUT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::with_capacity((length - start) as usize);
    file.take(ROLLOUT_TAIL_BYTES).read_to_end(&mut tail)?;
    let mut lines = tail.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if start > 0 && !lines.is_empty() {
        // The first line of a tail read starts mid-record.
        lines.remove(0);
    }
    for line in lines.into_iter().rev() {
        if line.len() > MAX_ROLLOUT_LINE || !contains(line, b"\"rate_limits\":{") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<RolloutLine>(line) else {
            continue;
        };
        let Some(limits) = record.payload.rate_limits else {
            continue;
        };
        let Ok(observed_at) = DateTime::parse_from_rfc3339(&record.timestamp) else {
            continue;
        };
        return Ok(Some((observed_at.to_utc(), limits)));
    }
    Ok(None)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Window length from Antigravity's `window` names ("5h", "weekly").
fn antigravity_minutes(window: &str) -> Option<u32> {
    let window = window.trim().to_ascii_lowercase();
    match window.as_str() {
        "weekly" | "week" | "7d" => Some(10_080),
        "daily" | "day" | "24h" => Some(1_440),
        _ => window
            .strip_suffix('h')
            .and_then(|hours| hours.parse::<u32>().ok())
            .filter(|hours| *hours > 0)
            .map(|hours| hours * 60),
    }
}

/// Antigravity reports "remaining" per bucket from its auth probe; converted to "used".
pub(crate) fn antigravity_reading(
    buckets: &[CliQuotaBucket],
    observed_at: Option<DateTime<Utc>>,
) -> Result<QuotaReading, UnavailableReason> {
    let mut windows = Vec::new();
    for bucket in buckets {
        let resets_at = match bucket.reset_time.as_deref() {
            Some(reset) => Some(
                DateTime::parse_from_rfc3339(reset)
                    .map_err(|_| UnavailableReason::UnexpectedResponse)?
                    .to_utc(),
            ),
            None => None,
        };
        windows.push(
            QuotaWindow::from_remaining(
                Some(&bucket.group),
                f64::from(bucket.remaining_percent),
                antigravity_minutes(&bucket.window),
                resets_at,
            )
            .map_err(|_| UnavailableReason::UnexpectedResponse)?,
        );
    }
    Ok(QuotaReading {
        plan: None,
        windows,
        observed_at,
    })
}

/// Claude names its windows; the scope and length each name stands for. Unknown names are
/// skipped rather than guessed.
pub(crate) fn claude_window(name: &str) -> Option<(Option<&'static str>, u32)> {
    match name {
        "five_hour" => Some((None, 300)),
        "seven_day" => Some((None, 10_080)),
        "seven_day_opus" => Some((Some("Opus"), 10_080)),
        "seven_day_sonnet" => Some((Some("Sonnet"), 10_080)),
        _ => None,
    }
}

#[derive(Deserialize)]
struct RateLimitFrame {
    #[serde(rename = "type")]
    kind: String,
    rate_limit_info: Option<RateLimitInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitInfo {
    unified_windows: Option<std::collections::BTreeMap<String, UnifiedWindow>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnifiedWindow {
    /// A fraction, 0–1 (S2, Claude Code 2.1.302).
    utilization: f64,
    resets_at: Option<i64>,
}

/// `Some` when `line` is a Claude `rate_limit_event` frame carrying windows: the reading, or why
/// it was rejected. The frame reports every window at once, so it replaces the shown snapshot.
pub(crate) fn claude_rate_limit_frame(
    line: &[u8],
) -> Option<Result<QuotaReading, UnavailableReason>> {
    if !contains(line, b"\"rate_limit_event\"") {
        return None;
    }
    let frame = serde_json::from_slice::<RateLimitFrame>(line).ok()?;
    if frame.kind != "rate_limit_event" {
        return None;
    }
    let windows = frame.rate_limit_info?.unified_windows?;
    let mut reading = QuotaReading {
        plan: None,
        windows: Vec::new(),
        observed_at: None,
    };
    for (name, window) in windows {
        let Some((scope, minutes)) = claude_window(&name) else {
            continue;
        };
        if !(0.0..=1.0).contains(&window.utilization) {
            return Some(Err(UnavailableReason::UnexpectedResponse));
        }
        let resets_at = match window.resets_at.map(epoch_seconds).transpose() {
            Ok(resets_at) => resets_at,
            Err(reason) => return Some(Err(reason)),
        };
        match QuotaWindow::new(scope, window.utilization * 100.0, Some(minutes), resets_at) {
            Ok(window) => reading.windows.push(window),
            Err(_) => return Some(Err(UnavailableReason::UnexpectedResponse)),
        }
    }
    // Shortest window first, like the other sources.
    reading
        .windows
        .sort_by_key(|window| (window.window_minutes, window.scope.clone()));
    Some(Ok(reading))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> String {
        crate::quota::fixture("codex/rollout-token-count.jsonl")
            .trim()
            .to_string()
    }

    #[test]
    fn rollout_tail_yields_the_newest_rate_limits_record() {
        let root = tempfile::tempdir().unwrap();
        let day = root.path().join("2026/10/21");
        std::fs::create_dir_all(&day).unwrap();
        let record = record();
        let older = record
            .replace("20:00:00.000Z", "19:00:00.000Z")
            .replace("42.0", "30.0");
        let filler = format!(
            "{{\"timestamp\":\"2026-10-21T22:46:00Z\",\"type\":\"response_item\",\"payload\":{{\"text\":\"{}\"}}}}",
            "x".repeat(300 * 1024)
        );
        // The newest record sits before a long unrelated line, inside the tail.
        std::fs::write(
            day.join("rollout-a.jsonl"),
            format!(
                "{older}\n{record}\n{{\"timestamp\":\"x\",\"payload\":{{\"rate_limits\":null}}}}\n"
            ),
        )
        .unwrap();
        // A larger file whose only record is outside its tail.
        std::fs::write(day.join("rollout-b.jsonl"), format!("{record}\n{filler}\n")).unwrap();
        std::fs::write(day.join("notes.jsonl"), &record).unwrap();
        let reading = latest_codex_rollout(root.path()).unwrap();
        assert_eq!(reading.plan.as_deref(), Some("plus"));
        assert_eq!(
            reading.observed_at.unwrap(),
            DateTime::parse_from_rfc3339("2026-10-21T20:00:00.000Z").unwrap()
        );
        assert_eq!(reading.windows.len(), 2);
        assert_eq!(reading.windows[0].used_percent, 42.0);
        assert_eq!(reading.windows[0].window_minutes, Some(300));
        assert_eq!(reading.windows[0].scope, None);
        assert_eq!(reading.windows[1].label(), "Weekly");
        assert_eq!(
            latest_codex_rollout(&root.path().join("missing")).unwrap_err(),
            UnavailableReason::NoChannel
        );
    }

    #[test]
    fn rollout_values_outside_the_range_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("rollout-x.jsonl"),
            record().replace("42.0", "142.0"),
        )
        .unwrap();
        assert_eq!(
            latest_codex_rollout(root.path()).unwrap_err(),
            UnavailableReason::UnexpectedResponse
        );
    }

    #[test]
    fn claude_rate_limit_event_reports_both_windows_as_percent_used() {
        let line = crate::quota::fixture("claude/rate-limit-event.jsonl");
        let reading = claude_rate_limit_frame(line.trim().as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(reading.windows.len(), 2);
        assert_eq!(reading.windows[0].label(), "5h");
        assert!((reading.windows[0].used_percent - 30.0).abs() < 1e-9);
        assert_eq!(reading.windows[1].label(), "Weekly");
        assert_eq!(
            reading.windows[1].resets_at,
            DateTime::from_timestamp(1_793_220_000, 0)
        );
        // Other frames are not rate-limit events.
        assert!(claude_rate_limit_frame(br#"{"type":"assistant","message":{}}"#).is_none());
        assert!(
            claude_rate_limit_frame(br#"{"type":"user","text":"\"rate_limit_event\""}"#).is_none()
        );
        // A fraction outside 0-1 or an unknown-only frame never becomes a number.
        let over = line.replace("\"utilization\":0.3", "\"utilization\":1.4");
        assert_eq!(
            claude_rate_limit_frame(over.trim().as_bytes()).unwrap(),
            Err(UnavailableReason::UnexpectedResponse)
        );
        let unknown = br#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"opal_window":{"utilization":0.2}}}}"#;
        assert!(
            claude_rate_limit_frame(unknown)
                .unwrap()
                .unwrap()
                .windows
                .is_empty()
        );
        let legacy = br#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour"}}"#;
        assert!(claude_rate_limit_frame(legacy).is_none());
    }

    #[test]
    fn antigravity_buckets_convert_remaining_to_used_with_typed_windows() {
        let buckets = [
            CliQuotaBucket {
                group: "Gemini Models".into(),
                window: "weekly".into(),
                remaining_percent: 94,
                reset_time: Some("2026-10-16T14:01:26Z".into()),
            },
            CliQuotaBucket {
                group: "Gemini Models".into(),
                window: "5h".into(),
                remaining_percent: 100,
                reset_time: None,
            },
            CliQuotaBucket {
                group: "Other".into(),
                window: "fortnightly".into(),
                remaining_percent: 0,
                reset_time: None,
            },
        ];
        let reading = antigravity_reading(&buckets, None).unwrap();
        assert_eq!(reading.windows[0].used_percent, 6.0);
        assert_eq!(reading.windows[0].label(), "Weekly");
        assert_eq!(reading.windows[0].scope.as_deref(), Some("Gemini Models"));
        assert_eq!(reading.windows[1].used_percent, 0.0);
        assert_eq!(reading.windows[1].label(), "5h");
        assert_eq!(reading.windows[2].window_minutes, None);
        assert!(reading.windows[2].is_exhausted());
        let mut invalid = buckets[0].clone();
        invalid.reset_time = Some("soon".into());
        assert_eq!(
            antigravity_reading(&[invalid], None).unwrap_err(),
            UnavailableReason::UnexpectedResponse
        );
    }
}
