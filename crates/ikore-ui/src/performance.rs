use std::path::PathBuf;
use std::time::Instant;

use gpui::Window;
use serde_json::json;

const DEFAULT_SAMPLES: usize = 140;
const DEFAULT_WARMUP_SAMPLES: usize = 20;
const MAX_SAMPLES: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchmarkScenario {
    Idle,
    Closed,
    ThreeStreams,
}
impl BenchmarkScenario {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "dock-closed" => Some(Self::Closed),
            "dock-three-streams" => Some(Self::ThreeStreams),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Closed => "dock-closed",
            Self::ThreeStreams => "dock-three-streams",
        }
    }
}

pub(crate) struct FrameLatencyProbe {
    output_path: PathBuf,
    samples: usize,
    warmup_samples: usize,
    observed_frames: usize,
    last_frame_at: Option<Instant>,
    frame_intervals_ms: Vec<f64>,
    scenario: BenchmarkScenario,
    fixture_frames: u64,
    fixture_updates: u64,
}

impl FrameLatencyProbe {
    fn from_environment() -> Option<Self> {
        let output_path =
            ikore_contracts::var_os_with_legacy("BENCHMARK_UI_REPORT").map(PathBuf::from)?;
        let scenario = BenchmarkScenario::parse(
            &ikore_contracts::var_with_legacy("BENCHMARK_UI_SCENARIO")
                .unwrap_or_else(|_| "idle".into()),
        )?;
        let samples = environment_count("BENCHMARK_UI_SAMPLES", DEFAULT_SAMPLES);
        let warmup_samples = environment_count("BENCHMARK_UI_WARMUP", DEFAULT_WARMUP_SAMPLES)
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
            scenario,
            fixture_frames: 0,
            fixture_updates: 0,
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
        let fixture_ready = self.scenario != BenchmarkScenario::ThreeStreams
            || (self.fixture_frames > self.warmup_samples as u64
                && self.fixture_updates == self.fixture_frames * 3);
        write_report(
            &self.output_path,
            &json!({
                "schemaVersion": 1,
                "status": "completed",
                "metric": "gpui_animation_frame_callback_interval_ms",
                "scope": "Interval between GPUI render callbacks; compositor presentation is not observed directly",
                "scenario": self.scenario.label(),
                "streamingFixture": self.scenario == BenchmarkScenario::ThreeStreams,
                "fixtureFrames": self.fixture_frames,
                "updatedConversations": self.fixture_updates,
                "fixtureReady": fixture_ready,
                "viewportWidth": f32::from(window.viewport_size().width),
                "viewportHeight": f32::from(window.viewport_size().height),
                "samples": self.frame_intervals_ms.len(),
                "warmupSamples": self.warmup_samples,
                "p95Ms": p95_ms,
                "p99Ms": p99_ms,
                "p50Ms": percentile(&self.frame_intervals_ms, 0.5),
                "thresholds": { "p95Ms": 8.33, "p99Ms": 16.67 },
                "passed": fixture_ready && p95_ms <= 8.33 && p99_ms <= 16.67,
                "rawMs": self.frame_intervals_ms,
            }),
        );
        true
    }

    pub(crate) fn fixture_frame(&mut self) -> Option<u64> {
        if self.scenario != BenchmarkScenario::ThreeStreams {
            return None;
        }
        let frame = self.fixture_frames;
        self.fixture_frames = self.fixture_frames.saturating_add(1);
        Some(frame)
    }

    pub(crate) fn record_fixture_updates(&mut self, updates: usize) {
        self.fixture_updates = self.fixture_updates.saturating_add(updates as u64);
    }
}

/// In-memory only: no provider, repository or credentials are touched by this workload.
pub(crate) fn apply_streaming_fixture(
    model: &mut crate::AppModel,
    frame: u64,
    cx: &mut gpui::Context<crate::AppModel>,
) -> usize {
    use ikore_core::chat_service::{
        ChatConversation, ChatMessage, ChatTransportKind, GetChatConversationResponse,
    };
    use ikore_core::terminals::CliModelSelectionSource;
    if frame == 0 {
        model.companion_dock.prepare_benchmark();
        model.chat.update(cx, |chat, _| {
            chat.project_id = Some("benchmark-project".into());
            for index in 0..3 {
                let id = format!("benchmark-chat-{index}");
                chat.store_conversation_snapshot(GetChatConversationResponse {
                    conversation: ChatConversation {
                        id: id.clone(),
                        project_id: "benchmark-project".into(),
                        connection_id: None,
                        provider_id: "fixture".into(),
                        model_id: "offline-fixture".into(),
                        transport: ChatTransportKind::Api,
                        cli_backend: None,
                        model_profile: None,
                        model_source: CliModelSelectionSource::Default,
                        title: format!("Streaming fixture {}", index + 1),
                        connection_available: false,
                        created_at: "2026-11-07T00:00:00Z".into(),
                        updated_at: "2026-11-07T00:00:00Z".into(),
                    },
                    messages: vec![ChatMessage {
                        id: format!("benchmark-message-{index}"),
                        conversation_id: id.clone(),
                        ordinal: 0,
                        role: "assistant".into(),
                        content: String::new(),
                        status: "streaming".into(),
                        execution_id: None,
                        generation_id: None,
                        last_sequence: 0,
                        input_tokens: None,
                        output_tokens: None,
                        estimated_cost_usd: None,
                        error_code: None,
                        error_message: None,
                        target_label: None,
                        model_evidence: None,
                        created_at: "2026-11-07T00:00:00Z".into(),
                        updated_at: "2026-11-07T00:00:00Z".into(),
                    }],
                    tool_calls: Vec::new(),
                });
            }
        });
        for index in 0..3 {
            model.ensure_chat_composer(&format!("benchmark-chat-{index}"), cx);
        }
    }
    model.chat.update(cx, |chat, _| {
        let mut updated = 0;
        for index in 0..3 {
            if let Some(message) = chat
                .messages_by_conversation
                .get_mut(&format!("benchmark-chat-{index}"))
                .and_then(|messages| messages.first_mut())
            {
                message.content.push_str(&format!(
                    "\n\n**Update** {index}: UTF-8 → `code` sample {frame}"
                ));
                updated += 1;
            }
        }
        updated
    })
}

pub(crate) fn start_probe() -> Option<FrameLatencyProbe> {
    FrameLatencyProbe::from_environment()
}

fn environment_count(suffix: &str, default: usize) -> usize {
    ikore_contracts::var_with_legacy(suffix)
        .ok()
        .as_deref()
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
    use gpui::AppContext;

    #[test]
    fn percentile_uses_nearest_rank_without_interpolation() {
        let values = (1..=100).map(f64::from).collect::<Vec<_>>();
        assert_eq!(percentile(&values, 0.95), 95.0);
        assert_eq!(percentile(&values, 0.99), 99.0);
        assert!(percentile(&[], 0.95).is_infinite());
    }

    #[test]
    fn benchmark_scenarios_are_explicit_and_bounded() {
        assert_eq!(
            BenchmarkScenario::parse("dock-three-streams"),
            Some(BenchmarkScenario::ThreeStreams)
        );
        assert_eq!(
            BenchmarkScenario::parse("dock-closed"),
            Some(BenchmarkScenario::Closed)
        );
        assert_eq!(BenchmarkScenario::parse("arbitrary"), None);
    }

    #[gpui::test]
    fn streaming_fixture_updates_three_conversations_without_a_backend(
        cx: &mut gpui::TestAppContext,
    ) {
        let model = cx.new(crate::AppModel::new_for_test);
        model.update(cx, |model, cx| {
            apply_streaming_fixture(model, 0, cx);
            assert_eq!(model.chat.read(cx).conversations.len(), 3);
            apply_streaming_fixture(model, 1, cx);
            for index in 0..3 {
                assert!(
                    model
                        .chat
                        .read(cx)
                        .messages_for(&format!("benchmark-chat-{index}"))[0]
                        .content
                        .ends_with("sample 1")
                );
            }
            assert!(model.core.is_none());
        });
    }
}
