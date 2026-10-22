//! Pace and projection for one usage window (ADR-028 Â§6). The projection is an estimate and is
//! withheld when too little of the window has elapsed or too few samples exist.

use super::model::QuotaWindow;
use chrono::{DateTime, Duration, Utc};

/// No projection before this share of the window has elapsed.
const MIN_ELAPSED_FRACTION: f64 = 0.10;
const MIN_SAMPLES: usize = 3;
const MIN_SAMPLE_SPAN: Duration = Duration::minutes(15);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    /// When the window would reach 100% at the current pace.
    pub exhausts_at: DateTime<Utc>,
    /// Whether that happens before the window resets on its own.
    pub before_reset: bool,
}

/// Linear fit of `used_percent` over the samples taken inside the current window. `None` when
/// the data cannot support an estimate, or usage is flat or falling.
pub fn project(
    window: &QuotaWindow,
    samples: &[(DateTime<Utc>, f64)],
    now: DateTime<Utc>,
) -> Option<Projection> {
    let start = window.starts_at()?;
    let resets_at = window.resets_at?;
    if window.elapsed_fraction(now)? < MIN_ELAPSED_FRACTION || window.is_exhausted() {
        return None;
    }
    let points = samples
        .iter()
        .filter(|(at, _)| *at >= start && *at <= now)
        .map(|(at, used)| ((*at - start).num_seconds() as f64 / 60.0, *used))
        .collect::<Vec<_>>();
    if points.len() < MIN_SAMPLES {
        return None;
    }
    let first = points.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
    let last = points
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    if last - first < MIN_SAMPLE_SPAN.num_minutes() as f64 {
        return None;
    }
    let count = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / count;
    let covariance = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    let variance = points
        .iter()
        .map(|(x, _)| (x - mean_x).powi(2))
        .sum::<f64>();
    let slope = covariance / variance;
    if !slope.is_finite() || slope <= 0.0 {
        return None;
    }
    let minutes_left = (100.0 - window.used_percent) / slope;
    let exhausts_at = now + Duration::seconds((minutes_left * 60.0).min(1.0e9) as i64);
    Some(Projection {
        exhausts_at,
        before_reset: exhausts_at < resets_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value).unwrap().to_utc()
    }

    fn window(used: f64) -> QuotaWindow {
        // 10:00 â†’ 15:00 window.
        QuotaWindow::new(None, used, Some(300), Some(at("2026-10-21T15:00:00Z"))).unwrap()
    }

    #[test]
    fn quota_projection_is_withheld_with_too_little_data() {
        let samples = [
            (at("2026-10-21T10:05:00Z"), 5.0),
            (at("2026-10-21T10:15:00Z"), 10.0),
            (at("2026-10-21T10:25:00Z"), 15.0),
        ];
        // Less than 10% of the window elapsed.
        assert_eq!(
            project(&window(15.0), &samples, at("2026-10-21T10:25:00Z")),
            None
        );
        let now = at("2026-10-21T11:00:00Z");
        // Too few samples.
        assert_eq!(project(&window(15.0), &samples[..2], now), None);
        // Enough samples, but spanning less than 15 minutes.
        let tight = [
            (at("2026-10-21T10:50:00Z"), 10.0),
            (at("2026-10-21T10:55:00Z"), 12.0),
            (at("2026-10-21T10:59:00Z"), 14.0),
        ];
        assert_eq!(project(&window(14.0), &tight, now), None);
        // Samples from the previous window are ignored.
        let old = [
            (at("2026-10-21T09:00:00Z"), 50.0),
            (at("2026-10-21T09:30:00Z"), 60.0),
            (at("2026-10-21T10:05:00Z"), 5.0),
        ];
        assert_eq!(project(&window(5.0), &old, now), None);
        // Flat usage never projects an exhaustion.
        let flat = [
            (at("2026-10-21T10:10:00Z"), 20.0),
            (at("2026-10-21T10:30:00Z"), 20.0),
            (at("2026-10-21T10:50:00Z"), 20.0),
        ];
        assert_eq!(project(&window(20.0), &flat, now), None);
        // No reset or length: nothing to project against.
        let unknown = QuotaWindow::new(None, 20.0, None, None).unwrap();
        assert_eq!(project(&unknown, &samples, now), None);
    }

    #[test]
    fn steady_burn_projects_exhaustion_before_the_reset() {
        // 1% per minute from 10:00; at 11:00 the window is 60% used and runs out at 11:40.
        let samples = [
            (at("2026-10-21T10:20:00Z"), 20.0),
            (at("2026-10-21T10:40:00Z"), 40.0),
            (at("2026-10-21T11:00:00Z"), 60.0),
        ];
        let projection = project(&window(60.0), &samples, at("2026-10-21T11:00:00Z")).unwrap();
        assert_eq!(projection.exhausts_at, at("2026-10-21T11:40:00Z"));
        assert!(projection.before_reset);

        // A slow burn lasts past the reset.
        let slow = [
            (at("2026-10-21T10:20:00Z"), 1.0),
            (at("2026-10-21T10:40:00Z"), 2.0),
            (at("2026-10-21T11:00:00Z"), 3.0),
        ];
        let projection = project(&window(3.0), &slow, at("2026-10-21T11:00:00Z")).unwrap();
        assert!(!projection.before_reset);
    }
}
