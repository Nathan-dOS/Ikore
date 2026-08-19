use tauri::Emitter;

use super::domain::{LlmEventEnvelope, LlmEventSink};

#[derive(Debug, Default)]
pub struct NoopEventSink;

impl LlmEventSink for NoopEventSink {
    fn emit(&self, _envelope: &LlmEventEnvelope) {}
}

#[derive(Clone)]
pub struct TauriEventSink {
    app_handle: tauri::AppHandle,
}

impl TauriEventSink {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }
}

impl LlmEventSink for TauriEventSink {
    fn emit(&self, envelope: &LlmEventEnvelope) {
        let _ = self.app_handle.emit(envelope.event.event_name(), envelope);
    }
}
