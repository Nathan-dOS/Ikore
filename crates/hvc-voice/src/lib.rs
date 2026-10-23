//! Local speech-to-text for Companion notes (ADR-029 V1–V8).
//!
//! HVC installs NVIDIA NeMo-Speech.cpp and the Nemotron 3.5 ASR model itself,
//! both pinned by version, URL, size and SHA-256 in [`runtime`]. A global
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

pub use capture::Recorder;
pub use earcon::Earcon;
pub use hotkey::{Hotkey, Modifiers, PushToTalk, PushToTalkEvent};
pub use runtime::{
    ModelRelease, RuntimeRelease, RuntimeStatus, SetupProgress, SetupStage, Transcript,
    VoiceRuntime,
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
