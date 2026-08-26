use super::domain::{LlmEventEnvelope, LlmEventSink};

#[derive(Debug, Default)]
pub struct NoopEventSink;

impl LlmEventSink for NoopEventSink {
    fn emit(&self, _envelope: &LlmEventEnvelope) {}
}
