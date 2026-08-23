use tokio::sync::broadcast;

use crate::chat_service::{ChatEventEnvelope, ChatEventSink};
use crate::llm::{LlmEventEnvelope, LlmEventSink};
use crate::workspace_service::{WorkspaceChangedEvent, WorkspaceEventSink};

pub const DEFAULT_EVENT_CAPACITY: usize = 1_024;

#[derive(Debug, Clone)]
pub enum AppEvent {
    WorkspaceChanged(WorkspaceChangedEvent),
    Llm(LlmEventEnvelope),
    Chat(ChatEventEnvelope),
}

impl AppEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::WorkspaceChanged(_) => crate::workspace_service::WORKSPACE_CHANGED_EVENT,
            Self::Llm(envelope) => envelope.event.event_name(),
            Self::Chat(envelope) => envelope.event_name(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppEventBus {
    sender: broadcast::Sender<AppEvent>,
}

impl Default for AppEventBus {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_CAPACITY)
    }
}

impl AppEventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: AppEvent) {
        let _ = self.sender.send(event);
    }
}

impl WorkspaceEventSink for AppEventBus {
    fn emit(&self, event: WorkspaceChangedEvent) {
        self.publish(AppEvent::WorkspaceChanged(event));
    }
}

impl LlmEventSink for AppEventBus {
    fn emit(&self, envelope: &LlmEventEnvelope) {
        self.publish(AppEvent::Llm(envelope.clone()));
    }
}

impl ChatEventSink for AppEventBus {
    fn emit(&self, envelope: &ChatEventEnvelope) {
        self.publish(AppEvent::Chat(envelope.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preserves_event_order() {
        let bus = AppEventBus::new(4);
        let mut receiver = bus.subscribe();
        for sequence in 1..=3 {
            bus.publish(AppEvent::WorkspaceChanged(WorkspaceChangedEvent {
                kind: "modify".into(),
                relative_path: format!("note-{sequence}.md"),
                timestamp: sequence.to_string(),
            }));
        }

        for sequence in 1..=3 {
            let AppEvent::WorkspaceChanged(event) = receiver.recv().await.unwrap() else {
                panic!("expected workspace event");
            };
            assert_eq!(event.relative_path, format!("note-{sequence}.md"));
        }
    }

    #[tokio::test]
    async fn reports_lag_for_snapshot_resynchronization() {
        let bus = AppEventBus::new(2);
        let mut receiver = bus.subscribe();
        for sequence in 0..8 {
            bus.publish(AppEvent::WorkspaceChanged(WorkspaceChangedEvent {
                kind: "modify".into(),
                relative_path: format!("{sequence}.md"),
                timestamp: sequence.to_string(),
            }));
        }

        assert!(matches!(
            receiver.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
    }
}
