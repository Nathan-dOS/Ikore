use std::path::PathBuf;
use std::time::Instant;

use gpui::Window;
use serde_json::json;

const DEFAULT_SAMPLES: usize = 140;
const DEFAULT_WARMUP_SAMPLES: usize = 20;
const MAX_SAMPLES: usize = 2_000;

pub(crate) struct FrameLatencyProbe {
    output_path: PathBuf,
    samples: usize,
    warmup_samples: usize,
    observed_frames: usize,
    last_frame_at: Option<Instant>,
    frame_intervals_ms: Vec<f64>,
}

impl FrameLatencyProbe {
    fn from_environment() -> Option<Self> {
        let output_path = std::env::var_os("HVC_BENCHMARK_UI_REPORT").map(PathBuf::from)?;
        let samples = environment_count("HVC_BENCHMARK_UI_SAMPLES", DEFAULT_SAMPLES);
        let warmup_samples = environment_count("HVC_BENCHMARK_UI_WARMUP", DEFAULT_WARMUP_SAMPLES)
            .min(samples.saturating_sub(1));
        write_report(
            &output_path,
            &json!({
                "schemaVersion": 1,
                "status": "configured",
                "samples": samples,
                "warmupSamples": warmup_samples,
            }),
        );
        Some(Self {
            output_path,
            samples,
            warmup_samples,
            observed_frames: 0,
            last_frame_at: None,
            frame_intervals_ms: Vec::with_capacity(samples - warmup_samples),
        })
    }

    pub(crate) fn observe_render(&mut self, window: &Window) -> bool {
        let now = Instant::now();
        if let Some(previous) = self.last_frame_at.replace(now) {
            if self.observed_frames >= self.warmup_samples {
                self.frame_intervals_ms
                    .push((now - previous).as_secs_f64() * 1_000.0);
            }
            self.observed_frames += 1;
        } else {
            write_report(
                &self.output_path,
                &json!({
                    "schemaVersion": 1,
                    "status": "measuring",
                    "samples": self.samples,
                    "warmupSamples": self.warmup_samples,
                }),
            );
        }

        if self.observed_frames < self.samples {
            window.request_animation_frame();
            return false;
        }

        self.frame_intervals_ms.sort_by(f64::total_cmp);
        let p95_ms = percentile(&self.frame_intervals_ms, 0.95);
        let p99_ms = percentile(&self.frame_intervals_ms, 0.99);
        write_report(
            &self.output_path,
            &json!({
                "schemaVersion": 1,
                "status": "completed",
                "metric": "gpui_animation_frame_callback_interval_ms",
                "scope": "Interval between GPUI render callbacks; compositor presentation is not observed directly",
                "samples": self.frame_intervals_ms.len(),
                "warmupSamples": self.warmup_samples,
                "p95Ms": p95_ms,
                "p99Ms": p99_ms,
                "thresholds": { "p95Ms": 8.33, "p99Ms": 16.67 },
                "passed": p95_ms <= 8.33 && p99_ms <= 16.67,
                "rawMs": self.frame_intervals_ms,
            }),
        );
        true
    }
}

pub(crate) fn start_probe() -> Option<FrameLatencyProbe> {
    FrameLatencyProbe::from_environment()
}

fn environment_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .min(MAX_SAMPLES)
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return f64::INFINITY;
    }
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn write_report(path: &std::path::Path, report: &serde_json::Value) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_vec_pretty(report).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank_without_interpolation() {
        let values = (1..=100).map(f64::from).collect::<Vec<_>>();
        assert_eq!(percentile(&values, 0.95), 95.0);
        assert_eq!(percentile(&values, 0.99), 99.0);
        assert!(percentile(&[], 0.95).is_infinite());
    }
}
