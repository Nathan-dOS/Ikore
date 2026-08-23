use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sha2::{Digest, Sha256};

use super::domain::{Clock, JitterSource};

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Default)]
pub struct StableJitter;

impl JitterSource for StableJitter {
    fn factor(&self, key: &str, attempt: u32) -> f64 {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        hasher.update(attempt.to_le_bytes());
        let bytes = hasher.finalize();
        let bucket = u16::from_le_bytes([bytes[0], bytes[1]]) as f64 / u16::MAX as f64;
        0.8 + bucket * 0.4
    }
}

pub fn cooldown_ms(backoff_level: u32, jitter_factor: f64) -> u64 {
    let exponent = backoff_level.min(16);
    let base_ms = 3_000_u64.saturating_mul(1_u64 << exponent).min(300_000);
    ((base_ms as f64 * jitter_factor.clamp(0.8, 1.2)).round() as u64).min(300_000)
}

pub fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<u64> {
    let normalized = value.trim();
    if let Ok(seconds) = normalized.parse::<u64>() {
        return Some(seconds.saturating_mul(1000).min(3_600_000));
    }
    let at = DateTime::parse_from_rfc2822(normalized)
        .ok()?
        .with_timezone(&Utc);
    let duration = at.signed_duration_since(now).num_milliseconds().max(0) as u64;
    Some(duration.min(3_600_000))
}

pub fn eligible_at(now: DateTime<Utc>, delay_ms: u64) -> DateTime<Utc> {
    now + ChronoDuration::milliseconds(delay_ms.min(3_600_000) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_is_bounded_and_jittered() {
        assert_eq!(cooldown_ms(0, 1.0), 3000);
        assert_eq!(cooldown_ms(20, 1.2), 300000);
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_date() {
        let now = DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(parse_retry_after("12", now), Some(12000));
        assert_eq!(
            parse_retry_after("Tue, 18 Aug 2026 12:00:05 GMT", now),
            Some(5000)
        );
    }
}
