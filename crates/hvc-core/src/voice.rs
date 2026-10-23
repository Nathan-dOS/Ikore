//! Voice dictation for Companion notes (ADR-029 V1–V8). Composes the
//! `hvc-voice` runtime, the global push-to-talk and the Notes Scribe: hold
//! the shortcut, speak, release; the transcript becomes a voice note in the
//! project open in HVC, and the Scribe structures it like any capture.
use crate::companion_notes::{CaptureNoteRequest, NoteScribeRuntime, NoteSource};
use crate::persistence_service::{PersistenceService, PersistenceServiceError};
use crate::{AppEvent, AppEventBus};
use futures::future::BoxFuture;
use hvc_voice::{
    Earcon, PushToTalk, PushToTalkEvent, Recorder, Transcript, VoiceError, VoiceRuntime,
};
pub use hvc_voice::{Hotkey, Modifiers, RuntimeStatus, SetupProgress, SetupStage};
use rusqlite::{OptionalExtension, params};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const PUSH_TO_TALK_KEY: &str = "voice.push_to_talk";
const HOTKEY_KEY: &str = "voice.hotkey";
/// Holds shorter than this are taps, not speech, and are discarded.
pub const MIN_UTTERANCE: Duration = Duration::from_millis(500);

/// Something changed; the UI reads [`VoiceService::status`] again.
#[derive(Debug, Clone)]
pub struct VoiceEvent;

/// Push-to-talk starts off; the shortcut defaults to Ctrl+Shift+F9.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceSettings {
    pub push_to_talk: bool,
    pub hotkey: Hotkey,
}

/// What the last dictation did, for the UI's status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Dictation {
    #[default]
    Idle,
    Recording,
    Transcribing,
    Saved {
        project_id: String,
        note_id: String,
    },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceStatus {
    pub runtime: RuntimeStatus,
    pub runtime_version: String,
    pub model_name: String,
    pub model_bytes: u64,
    pub setup: Option<SetupProgress>,
    pub setup_error: Option<String>,
    pub settings: VoiceSettings,
    /// The shortcut is registered and listening.
    pub listening: bool,
    pub hotkey_error: Option<String>,
    pub dictation: Dictation,
}

impl VoiceStatus {
    /// What to show before the first status read: nothing installed yet.
    pub fn checking() -> Self {
        let release = hvc_voice::RuntimeRelease::pinned();
        let model = hvc_voice::ModelRelease::pinned();
        Self {
            runtime: RuntimeStatus {
                platform_supported: cfg!(windows),
                runtime_ready: false,
                model_ready: false,
            },
            runtime_version: release.version,
            model_name: model.name,
            model_bytes: model.artifact.bytes,
            setup: None,
            setup_error: None,
            settings: VoiceSettings::default(),
            listening: false,
            hotkey_error: None,
            dictation: Dictation::Idle,
        }
    }
}

/// Turns recorded speech into text; the runtime in production, a fixture in tests.
pub(crate) trait Transcriber: Send + Sync {
    fn transcribe<'a>(
        &'a self,
        wav: Vec<u8>,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Transcript, VoiceError>>;
}

impl Transcriber for VoiceRuntime {
    fn transcribe<'a>(
        &'a self,
        wav: Vec<u8>,
        cancellation: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Transcript, VoiceError>> {
        Box::pin(async move { VoiceRuntime::transcribe(self, &wav, cancellation).await })
    }
}

#[derive(Default)]
struct State {
    active_project: Option<String>,
    setup: Option<CancellationToken>,
    setup_progress: Option<SetupProgress>,
    setup_error: Option<String>,
    push_to_talk: Option<PushToTalk>,
    hotkey_error: Option<String>,
    recorder: Option<Recorder>,
    dictation: Dictation,
    closing: bool,
}

pub struct VoiceService {
    runtime: VoiceRuntime,
    persistence: PersistenceService,
    events: Arc<AppEventBus>,
    handle: tokio::runtime::Handle,
    notes: Arc<NoteScribeRuntime>,
    state: Mutex<State>,
    me: Weak<Self>,
    transcriber_override: Option<Arc<dyn Transcriber>>,
    sound: fn(Earcon),
}

impl VoiceService {
    pub fn new(
        runtime: VoiceRuntime,
        persistence: PersistenceService,
        events: Arc<AppEventBus>,
        handle: tokio::runtime::Handle,
        notes: Arc<NoteScribeRuntime>,
    ) -> Arc<Self> {
        Self::build(
            runtime,
            persistence,
            events,
            handle,
            notes,
            None,
            hvc_voice::earcon::play,
        )
    }

    /// `new` with the transcriber and the sound player replaceable, for tests.
    pub(crate) fn build(
        runtime: VoiceRuntime,
        persistence: PersistenceService,
        events: Arc<AppEventBus>,
        handle: tokio::runtime::Handle,
        notes: Arc<NoteScribeRuntime>,
        transcriber_override: Option<Arc<dyn Transcriber>>,
        sound: fn(Earcon),
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            runtime,
            persistence,
            events,
            handle,
            notes,
            state: Mutex::new(State::default()),
            me: me.clone(),
            transcriber_override,
            sound,
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn changed(&self) {
        self.events.publish(AppEvent::Voice(VoiceEvent));
    }

    pub fn status(&self) -> VoiceStatus {
        let settings = self.settings().unwrap_or_default();
        let state = self.state();
        VoiceStatus {
            runtime: self.runtime.status(),
            runtime_version: self.runtime.release().version.clone(),
            model_name: self.runtime.model().name.clone(),
            model_bytes: self.runtime.model().artifact.bytes,
            setup: state.setup_progress,
            setup_error: state.setup_error.clone(),
            settings,
            listening: state.push_to_talk.is_some(),
            hotkey_error: state.hotkey_error.clone(),
            dictation: state.dictation.clone(),
        }
    }

    pub fn settings(&self) -> Result<VoiceSettings, PersistenceServiceError> {
        let db = self.persistence.connection()?;
        let read = |key: &str| -> Result<Option<String>, PersistenceServiceError> {
            Ok(db
                .query_row(
                    "SELECT value FROM runtime_settings WHERE key=?1",
                    [key],
                    |row| row.get(0),
                )
                .optional()?)
        };
        Ok(VoiceSettings {
            push_to_talk: read(PUSH_TO_TALK_KEY)?.as_deref() == Some("true"),
            hotkey: read(HOTKEY_KEY)?
                .and_then(|id| Hotkey::parse(&id).ok())
                .unwrap_or_default(),
        })
    }

    /// Stores the settings and registers or releases the shortcut to match.
    pub fn save_settings(
        &self,
        settings: VoiceSettings,
    ) -> Result<VoiceStatus, PersistenceServiceError> {
        let db = self.persistence.connection()?;
        let now = chrono::Utc::now().to_rfc3339();
        for (key, value) in [
            (PUSH_TO_TALK_KEY, settings.push_to_talk.to_string()),
            (HOTKEY_KEY, settings.hotkey.id()),
        ] {
            db.execute(
                "INSERT INTO runtime_settings(key,value,updated_at) VALUES(?1,?2,?3)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
                params![key, value, now],
            )?;
        }
        drop(db);
        self.apply();
        Ok(self.status())
    }

    /// The project voice notes go to: the one open in HVC.
    pub fn set_active_project(&self, project_id: Option<String>) {
        self.state().active_project = project_id;
    }

    /// Registers the shortcut when push-to-talk is on and everything it needs
    /// is installed; releases it otherwise. Called at start-up, after setup
    /// and whenever the settings change.
    pub fn apply(&self) {
        let settings = self.settings().unwrap_or_default();
        let wanted = settings.push_to_talk && self.runtime.status().ready();
        let previous = {
            let mut state = self.state();
            state.hotkey_error = None;
            state.push_to_talk.take()
        };
        // Dropped outside the lock: it joins the shortcut thread, whose
        // callback takes the same lock.
        drop(previous);
        if wanted && !self.state().closing {
            let me = self.me.clone();
            let started = PushToTalk::start(settings.hotkey, move |event| {
                if let Some(service) = me.upgrade() {
                    service.on_push_to_talk(event);
                }
            });
            let mut state = self.state();
            match started {
                Ok(push_to_talk) => state.push_to_talk = Some(push_to_talk),
                Err(error) => state.hotkey_error = Some(error.to_string()),
            }
        }
        self.changed();
    }

    /// Installs the runtime, then downloads the model, reporting progress.
    /// Returns at once; a second call while one runs does nothing.
    pub fn start_setup(&self) {
        let token = CancellationToken::new();
        {
            let mut state = self.state();
            if state.setup.is_some() || state.closing {
                return;
            }
            state.setup = Some(token.clone());
            state.setup_error = None;
            state.setup_progress = None;
        }
        self.changed();
        let me = self.me.clone();
        self.handle.spawn(async move {
            let Some(service) = me.upgrade() else { return };
            let report = {
                let me = me.clone();
                move |progress: SetupProgress| {
                    if let Some(service) = me.upgrade() {
                        service.state().setup_progress = Some(progress);
                        service.changed();
                    }
                }
            };
            let outcome = match service.runtime.install_runtime(&report, &token).await {
                Ok(()) => service.runtime.download_model(&report, &token).await,
                Err(error) => Err(error),
            };
            {
                let mut state = service.state();
                state.setup = None;
                state.setup_progress = None;
                state.setup_error = match outcome {
                    Ok(()) | Err(VoiceError::Cancelled) => None,
                    Err(error) => Some(error.to_string()),
                };
            }
            service.apply();
        });
    }

    pub fn cancel_setup(&self) {
        if let Some(token) = &self.state().setup {
            token.cancel();
        }
    }

    /// Stops listening and cancels setup (application shutdown).
    pub fn shutdown(&self) {
        let (push_to_talk, recorder) = {
            let mut state = self.state();
            state.closing = true;
            if let Some(token) = &state.setup {
                token.cancel();
            }
            (state.push_to_talk.take(), state.recorder.take())
        };
        drop(push_to_talk);
        drop(recorder);
    }

    fn set_dictation(&self, dictation: Dictation) {
        self.state().dictation = dictation;
        self.changed();
    }

    fn fail(&self, reason: impl Into<String>) {
        (self.sound)(Earcon::Failed);
        self.set_dictation(Dictation::Failed(reason.into()));
    }

    /// Runs on the shortcut thread; keeps the press path short.
    fn on_push_to_talk(&self, event: PushToTalkEvent) {
        match event {
            PushToTalkEvent::Pressed => {
                if self.state().active_project.is_none() {
                    self.fail("No project is open in HVC.");
                    return;
                }
                match Recorder::start() {
                    Ok(recorder) => {
                        self.state().recorder = Some(recorder);
                        (self.sound)(Earcon::Start);
                        self.set_dictation(Dictation::Recording);
                    }
                    Err(error) => self.fail(error.to_string()),
                }
            }
            PushToTalkEvent::Released { held } => {
                let Some(recorder) = self.state().recorder.take() else {
                    return;
                };
                let samples = match recorder.stop() {
                    Ok(samples) => samples,
                    Err(error) => return self.fail(error.to_string()),
                };
                let Some(service) = self.me.upgrade() else {
                    return;
                };
                self.handle.spawn(async move {
                    service.finish_dictation(samples, held).await;
                });
            }
        }
    }

    /// Transcribes one utterance and stores it as a voice note. Public to the
    /// crate so tests can drive it without a microphone.
    pub(crate) async fn finish_dictation(&self, samples: Vec<i16>, held: Duration) {
        if held < MIN_UTTERANCE {
            return self.fail("The shortcut was released too quickly to hear anything.");
        }
        let Some(project_id) = self.state().active_project.clone() else {
            return self.fail("No project is open in HVC.");
        };
        self.set_dictation(Dictation::Transcribing);
        let wav = hvc_voice::wav::encode_mono_16(&samples, hvc_voice::capture::SAMPLE_RATE);
        let cancellation = CancellationToken::new();
        let transcribed = match &self.transcriber_override {
            Some(transcriber) => transcriber.transcribe(wav, &cancellation).await,
            None => Transcriber::transcribe(&self.runtime, wav, &cancellation).await,
        };
        let text = match transcribed {
            Ok(transcript) if transcript.text.is_empty() => {
                return self.fail("No speech was recognized.");
            }
            Ok(transcript) => transcript.text,
            Err(error) => return self.fail(error.to_string()),
        };
        let notes = self.notes.clone();
        let request = CaptureNoteRequest {
            project_id: project_id.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            raw_input: text,
            source_type: NoteSource::Voice,
        };
        match tokio::task::spawn_blocking(move || notes.capture(request)).await {
            Ok(Ok(note)) => {
                (self.sound)(Earcon::Saved);
                self.set_dictation(Dictation::Saved {
                    project_id,
                    note_id: note.id,
                });
            }
            Ok(Err(error)) => self.fail(error.to_string()),
            Err(error) => self.fail(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion_notes::{CompanionNotesService, NoteStatus};
    use crate::persistence_service::EnsureProjectForWorkspaceRequest;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SOUNDS: Mutex<Vec<Earcon>> = Mutex::new(Vec::new());
    fn record_sound(earcon: Earcon) {
        SOUNDS.lock().unwrap().push(earcon);
    }

    struct Fixed(&'static str, AtomicUsize);
    impl Transcriber for Fixed {
        fn transcribe<'a>(
            &'a self,
            wav: Vec<u8>,
            _cancellation: &'a CancellationToken,
        ) -> BoxFuture<'a, Result<Transcript, VoiceError>> {
            Box::pin(async move {
                assert_eq!(&wav[0..4], b"RIFF", "the runtime receives a WAV image");
                self.1.fetch_add(1, Ordering::SeqCst);
                Ok(Transcript {
                    text: self.0.into(),
                    languages: vec!["pt-BR".into()],
                    duration_secs: 1.0,
                })
            })
        }
    }

    struct Fixture {
        _data: tempfile::TempDir,
        _workspace: tempfile::TempDir,
        persistence: PersistenceService,
        project: String,
        notes: Arc<NoteScribeRuntime>,
        events: Arc<AppEventBus>,
    }

    impl Fixture {
        fn new() -> Self {
            let data = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            let persistence = PersistenceService::initialize(data.path()).unwrap();
            let project = persistence
                .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                    workspace_path: workspace.path().to_string_lossy().into(),
                    preferred_name: None,
                })
                .unwrap()
                .project
                .id;
            let events = Arc::new(AppEventBus::default());
            // No gateway: the Scribe's structuring fails in the background,
            // which these tests do not wait for. The capture is what counts.
            let notes = Arc::new(
                NoteScribeRuntime::new(
                    persistence.clone(),
                    events.clone(),
                    tokio::runtime::Handle::current(),
                )
                .unwrap(),
            );
            Self {
                _data: data,
                _workspace: workspace,
                persistence,
                project,
                notes,
                events,
            }
        }

        fn service(&self, text: &'static str) -> (Arc<VoiceService>, Arc<Fixed>) {
            let fixed = Arc::new(Fixed(text, AtomicUsize::new(0)));
            let service = VoiceService::build(
                VoiceRuntime::new(self._data.path().join("voice")),
                self.persistence.clone(),
                self.events.clone(),
                tokio::runtime::Handle::current(),
                self.notes.clone(),
                Some(fixed.clone()),
                record_sound,
            );
            (service, fixed)
        }
    }

    #[tokio::test]
    async fn settings_default_off_and_round_trip() {
        let f = Fixture::new();
        let (service, _) = f.service("unused");
        assert_eq!(service.settings().unwrap(), VoiceSettings::default());
        let hotkey = Hotkey::parse("ctrl+alt+f2").unwrap();
        let status = service
            .save_settings(VoiceSettings {
                push_to_talk: true,
                hotkey,
            })
            .unwrap();
        assert_eq!(status.settings.hotkey, hotkey);
        assert!(status.settings.push_to_talk);
        assert!(
            !status.listening,
            "nothing is installed, so the shortcut is not registered"
        );
        assert!(!status.runtime.ready());
        assert_eq!(status.model_name, "Nemotron 3.5 ASR Streaming 0.6B");
    }

    #[tokio::test]
    async fn a_dictation_becomes_a_voice_note_in_the_open_project() {
        let f = Fixture::new();
        let (service, fixed) = f.service("O bot\u{e3}o de salvar some no tema escuro");
        service.set_active_project(Some(f.project.clone()));
        SOUNDS.lock().unwrap().clear();
        service
            .finish_dictation(vec![0; 16_000], Duration::from_secs(2))
            .await;
        let Dictation::Saved {
            project_id,
            note_id,
        } = service.status().dictation
        else {
            panic!("saved, got {:?}", service.status().dictation);
        };
        assert_eq!(project_id, f.project);
        let note = CompanionNotesService::new(f.persistence.clone())
            .get(&f.project, &note_id)
            .unwrap();
        assert_eq!(note.source_type, NoteSource::Voice);
        assert_eq!(note.raw_input, "O bot\u{e3}o de salvar some no tema escuro");
        assert!(matches!(
            note.status,
            NoteStatus::Structuring | NoteStatus::Failed
        ));
        assert_eq!(fixed.1.load(Ordering::SeqCst), 1);
        assert!(SOUNDS.lock().unwrap().contains(&Earcon::Saved));
    }

    #[tokio::test]
    async fn taps_silence_and_a_missing_project_store_nothing_and_say_so() {
        let f = Fixture::new();
        let notes = CompanionNotesService::new(f.persistence.clone());

        let (service, fixed) = f.service("ignored");
        service.set_active_project(Some(f.project.clone()));
        service
            .finish_dictation(vec![0; 1_000], Duration::from_millis(200))
            .await;
        assert!(matches!(service.status().dictation, Dictation::Failed(_)));
        assert_eq!(
            fixed.1.load(Ordering::SeqCst),
            0,
            "a tap is never transcribed"
        );

        let (service, _) = f.service("");
        service.set_active_project(Some(f.project.clone()));
        service
            .finish_dictation(vec![0; 16_000], Duration::from_secs(1))
            .await;
        let Dictation::Failed(reason) = service.status().dictation else {
            panic!("failed");
        };
        assert!(reason.contains("No speech"), "{reason}");

        let (service, _) = f.service("Something");
        service
            .finish_dictation(vec![0; 16_000], Duration::from_secs(1))
            .await;
        let Dictation::Failed(reason) = service.status().dictation else {
            panic!("failed");
        };
        assert!(reason.contains("No project"), "{reason}");
        assert!(notes.snapshot(&f.project).unwrap().notes.is_empty());
    }
}
