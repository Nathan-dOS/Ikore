//! Local speech-to-text for Companion notes (ADR-029 V1–V8).
//!
//! HVC installs NVIDIA NeMo-Speech.cpp with Nemotron, or native whisper.cpp
//! with Whisper large-v3-turbo `Q5_0` and Portuguese recognition. Models are pinned by URL,
//! size and SHA-256 in [`runtime`]. A global
//! push-to-talk shortcut ([`hotkey`]) records the default microphone at
//! 16 kHz ([`capture`]) while it is held; one runtime process then transcribes
//! the utterance, and short tones ([`earcon`]) report the outcome. Audio is
//! never stored. This crate knows nothing about notes, projects or the UI:
//! the host composes it.
pub mod capture;
pub mod earcon;
pub mod hotkey;
pub mod runtime;
pub mod wav;
pub mod whisper;

/// Selectable local recognizers. Existing installations keep Nemotron by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VoiceBackend {
    #[default]
    Nemotron,
    WhisperLargeV3Turbo,
}

impl VoiceBackend {
    pub const ALL: [Self; 2] = [Self::Nemotron, Self::WhisperLargeV3Turbo];

    pub fn id(self) -> &'static str {
        match self {
            Self::Nemotron => "nemotron",
            Self::WhisperLargeV3Turbo => "whisper-large-v3-turbo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nemotron => "Nemotron 3.5 ASR",
            Self::WhisperLargeV3Turbo => "Whisper large-v3-turbo Q5_0 · Portuguese",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        if value == "whisper-medium" {
            return Some(Self::WhisperLargeV3Turbo);
        }
        Self::ALL.into_iter().find(|backend| backend.id() == value)
    }
}

pub use capture::Recorder;
pub use earcon::Earcon;
pub use hotkey::{Hotkey, Modifiers, PushToTalk, PushToTalkEvent};
pub use runtime::{
    ModelRelease, RuntimeRelease, RuntimeStatus, ScratchSweep, SetupProgress, SetupStage,
    Transcript, VoiceRuntime,
};

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("voice dictation is available on Windows only")]
    Unsupported,
    #[error("the voice runtime or model is not installed")]
    NotInstalled,
    #[error("download failed: {0}")]
    Network(String),
    #[error("download rejected: {0}")]
    Integrity(String),
    #[error("{0}")]
    Runtime(String),
    #[error("microphone: {0}")]
    Capture(String),
    #[error("shortcut: {0}")]
    Hotkey(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod backend_tests {
    use super::VoiceBackend;

    #[test]
    fn whisper_selection_migrates_without_changing_the_default_backend() {
        assert_eq!(VoiceBackend::default(), VoiceBackend::Nemotron);
        assert_eq!(
            VoiceBackend::parse("whisper-medium"),
            Some(VoiceBackend::WhisperLargeV3Turbo)
        );
        assert_eq!(
            VoiceBackend::parse("whisper-large-v3-turbo"),
            Some(VoiceBackend::WhisperLargeV3Turbo)
        );
        assert_eq!(
            VoiceBackend::WhisperLargeV3Turbo.id(),
            "whisper-large-v3-turbo"
        );
        assert_eq!(VoiceBackend::ALL.len(), 2);
        assert_eq!(VoiceBackend::parse("unknown"), None);
    }
}
