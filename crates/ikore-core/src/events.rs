use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::chat_service::{ChatEventEnvelope, ChatEventSink};
use crate::embedding_service::{ModelPullEvent, ModelPullEventSink};
use crate::llm::{LlmEventEnvelope, LlmEventSink};
use crate::orchestration::{IndexingEvent, IndexingEventSink};
use crate::persistence_service::PersistenceService;
use crate::terminals::{TerminalEventEnvelope, TerminalEventSink};
use crate::workspace_service::{WorkspaceChangedEvent, WorkspaceEventSink};

pub const DEFAULT_EVENT_CAPACITY: usize = 1_024;

#[derive(Debug, Clone)]
pub enum AppEvent {
    Forge(crate::feature_forge::ForgeEvent),
    /// A Companion note changed (ADR-029); the UI reloads its snapshot.
    Notes(crate::companion_notes::NotesEvent),
    /// Project drawing changed; subscribers reload their sanitized snapshot.
    Drawings(crate::companion_drawings::service::DrawingsEvent),
    /// Voice setup or dictation changed (ADR-029); the UI rereads the status.
    Voice(crate::voice::VoiceEvent),
    /// A documentation reconciliation proposal finished (ADR-034 M5).
    DocDrift(crate::doc_reconciliation::DocDriftEvent),
    WorkspaceChanged(WorkspaceChangedEvent),
    Llm(LlmEventEnvelope),
    Chat(ChatEventEnvelope),
    CanvasAuthoring(crate::chat_service::canvas_authoring::CanvasAuthoringChangedV1),
    Terminal(Box<TerminalEventEnvelope>),
    Indexing(IndexingEvent),
    ModelPull(ModelPullEvent),
    /// An exchange log item was committed (ADR-037 M2).
    Exchange(crate::exchange::log::ExchangeDoorbellV1),
}

/// How a subscriber that missed an event recovers it (ADR-037 M2). Every
/// variant has a class; a new variant does not compile until it gets one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventClass {
    /// Its domain write appends a log item to a stream of this kind; a
    /// follower reads it back from its cursor.
    Durable(crate::exchange::log::StreamKind),
    /// Has a durable sequence of its own and a ranged re-read (terminal
    /// events, chat message sequences); a run's status is durable apart,
    /// in its `terminal_run` stream.
    OwnSequence,
    /// Broadcast only; its source is re-read when needed.
    Ephemeral,
    /// The doorbell itself; never recorded.
    Doorbell,
}

impl AppEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::Forge(_) => "forge://changed",
            Self::Notes(_) => "notes://changed",
            Self::Drawings(_) => "drawings://changed",
            Self::Voice(_) => "voice://changed",
            Self::DocDrift(_) => "docs://drift",
            Self::WorkspaceChanged(_) => crate::workspace_service::WORKSPACE_CHANGED_EVENT,
            Self::Llm(envelope) => envelope.event.event_name(),
            Self::Chat(envelope) => envelope.event_name(),
            Self::CanvasAuthoring(_) => "canvas-authoring://changed",
            Self::Terminal(envelope) => envelope.event_name(),
            Self::Indexing(event) => match event {
                IndexingEvent::Progress { .. } => "indexing://progress",
                IndexingEvent::Paused { .. } => "indexing://paused",
                IndexingEvent::Idle { .. } => "indexing://idle",
                IndexingEvent::Failed { .. } => "indexing://failed",
            },
            Self::ModelPull(event) => match event {
                ModelPullEvent::Started { .. } => "model_pull://started",
                ModelPullEvent::Progress { .. } => "model_pull://progress",
                ModelPullEvent::Completed { .. } => "model_pull://completed",
                ModelPullEvent::Failed { .. } => "model_pull://failed",
            },
            Self::Exchange(_) => "exchange://doorbell",
        }
    }

    /// The event inventory of the Agent Exchange specification, section 2.
    pub fn class(&self) -> EventClass {
        use crate::chat_service::ChatEventKind;
        use crate::exchange::log::StreamKind;
        match self {
            Self::Forge(_) => EventClass::Durable(StreamKind::Forge),
            Self::Notes(_) => EventClass::Durable(StreamKind::Notes),
            Self::Drawings(_) => EventClass::Durable(StreamKind::Drawings),
            Self::DocDrift(_) => EventClass::Durable(StreamKind::Docs),
            Self::CanvasAuthoring(_) => EventClass::Durable(StreamKind::ChatConversation),
            Self::Chat(envelope) => match envelope.kind {
                ChatEventKind::TurnCompleted
                | ChatEventKind::TurnFailed
                | ChatEventKind::TurnCancelled => EventClass::Durable(StreamKind::ChatConversation),
                _ => EventClass::OwnSequence,
            },
            Self::Terminal(_) => EventClass::OwnSequence,
            Self::Llm(_)
            | Self::Indexing(_)
            | Self::ModelPull(_)
            | Self::Voice(_)
            | Self::WorkspaceChanged(_) => EventClass::Ephemeral,
            Self::Exchange(_) => EventClass::Doorbell,
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

pub(crate) struct MaintenanceWorkspaceEventSink {
    events: Arc<AppEventBus>,
    persistence: PersistenceService,
    workspace_root: PathBuf,
}

impl MaintenanceWorkspaceEventSink {
    pub(crate) fn new(
        events: Arc<AppEventBus>,
        persistence: PersistenceService,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            events,
            persistence,
            workspace_root,
        }
    }
}

impl WorkspaceEventSink for MaintenanceWorkspaceEventSink {
    fn emit(&self, event: WorkspaceChangedEvent) {
        if let Err(error) = self
            .persistence
            .record_workspace_change(&self.workspace_root, &event)
        {
            eprintln!("workspace maintenance event ignored safely: {error}");
        }
        self.events.publish(AppEvent::WorkspaceChanged(event));
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
    fn emit_canvas(&self, event: &crate::chat_service::canvas_authoring::CanvasAuthoringChangedV1) {
        self.publish(AppEvent::CanvasAuthoring(event.clone()));
    }
}

impl TerminalEventSink for AppEventBus {
    fn emit(&self, envelope: &TerminalEventEnvelope) {
        self.publish(AppEvent::Terminal(Box::new(envelope.clone())));
    }
}

impl IndexingEventSink for AppEventBus {
    fn emit(&self, event: &IndexingEvent) {
        self.publish(AppEvent::Indexing(event.clone()));
    }
}

impl ModelPullEventSink for AppEventBus {
    fn emit(&self, event: &ModelPullEvent) {
        self.publish(AppEvent::ModelPull(event.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence_service::CreateProjectRequest;
    use std::fs;

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

    /// The event inventory of ADR-037 (specification, section 2). `class`
    /// matches every variant, so a new one does not compile without a class;
    /// this checks the classes the inventory assigns.
    #[test]
    fn every_event_family_has_its_inventory_class() {
        use crate::exchange::log::{ExchangeDoorbellV1, StreamKind};
        let forge = AppEvent::Forge(crate::feature_forge::ForgeEvent {
            project_id: "p".into(),
            feature_id: None,
        });
        let notes = AppEvent::Notes(crate::companion_notes::NotesEvent {
            project_id: "p".into(),
            note_id: None,
        });
        let drawings = AppEvent::Drawings(crate::companion_drawings::service::DrawingsEvent {
            project_id: "p".into(),
            drawing_id: "d".into(),
            revision: 1,
        });
        let docs = AppEvent::DocDrift(crate::doc_reconciliation::DocDriftEvent {
            project_id: "p".into(),
        });
        let workspace = AppEvent::WorkspaceChanged(WorkspaceChangedEvent {
            kind: "modify".into(),
            relative_path: "a.md".into(),
            timestamp: "now".into(),
        });
        let doorbell = AppEvent::Exchange(ExchangeDoorbellV1 {
            project_id: "p".into(),
            stream_id: "s".into(),
            stream_kind: StreamKind::Forge,
            subject_id: "project".into(),
            kind: "forge_changed".into(),
            seq: 1,
            global_seq: 1,
            previous_global_seq: 0,
        });
        assert_eq!(forge.class(), EventClass::Durable(StreamKind::Forge));
        assert_eq!(notes.class(), EventClass::Durable(StreamKind::Notes));
        assert_eq!(drawings.class(), EventClass::Durable(StreamKind::Drawings));
        assert_eq!(docs.class(), EventClass::Durable(StreamKind::Docs));
        assert_eq!(workspace.class(), EventClass::Ephemeral);
        assert_eq!(doorbell.class(), EventClass::Doorbell);
        assert_eq!(doorbell.event_name(), "exchange://doorbell");
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

    #[tokio::test]
    async fn maintenance_sink_persists_external_code_change_before_forwarding_it() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(workspace.path().join("src/lib.rs"), "fn external() {}\n").unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("External watcher".into()),
            })
            .unwrap()
            .project;
        persistence
            .set_brownfield_onboarding_consent(&project.id, true)
            .unwrap();
        let events = Arc::new(AppEventBus::new(4));
        let mut receiver = events.subscribe();
        let sink = MaintenanceWorkspaceEventSink::new(
            events,
            persistence.clone(),
            workspace.path().to_owned(),
        );
        sink.emit(WorkspaceChangedEvent {
            kind: "create".into(),
            relative_path: "src/lib.rs".into(),
            timestamp: "now".into(),
        });

        let frontier = persistence.list_brownfield_frontier(&project.id).unwrap();
        assert_eq!(frontier.len(), 1);
        assert_eq!(frontier[0].relative_path, "src/lib.rs");
        let AppEvent::WorkspaceChanged(forwarded) = receiver.recv().await.unwrap() else {
            panic!("workspace event expected");
        };
        assert_eq!(forwarded.relative_path, "src/lib.rs");
    }
}
