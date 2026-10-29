//! The speech-to-text runtimes HVC installs and drives (ADR-029 V1–V3):
//! NVIDIA NeMo-Speech.cpp with Nemotron, or native whisper.cpp with Whisper
//! large-v3-turbo `Q5_0`. Runtime and model artifacts are pinned here by
//! version, URL, size and SHA-256. One process transcribes one utterance;
//! nothing stays resident.
use crate::{VoiceBackend, VoiceError, whisper};
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// A file HVC downloads and accepts only with this exact size and digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

/// The runtime release: a zip with one top-level folder and backend-specific executable layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRelease {
    pub version: String,
    pub archive: Artifact,
    pub archive_root: String,
}

/// A downloadable local recognition model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRelease {
    pub name: String,
    pub file_name: String,
    pub artifact: Artifact,
}

impl RuntimeRelease {
    /// Official Windows x64 CPU binaries built from whisper.cpp 1.9.5.
    pub fn whisper_cpp() -> Self {
        Self {
            version: whisper::VERSION.into(),
            archive: Artifact {
                url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip".into(),
                sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb".into(),
                bytes: 8_928_640,
            },
            archive_root: "Release".into(),
        }
    }

    /// NeMo-Speech.cpp 0.2.0, Windows x64, CPU backend (checked 2026-10-23).
    pub fn pinned() -> Self {
        Self {
            version: "0.2.0".into(),
            archive: Artifact {
                url: "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-windows-x86_64-cpu.zip".into(),
                sha256: "82c80451086e86194aba9af19f800da77cd66d1c28d993d0efceffd6cdb49bb8".into(),
                bytes: 5_528_418,
            },
            archive_root: "nemo-speech-0.2.0-windows-x86_64-cpu".into(),
        }
    }
}

impl ModelRelease {
    pub fn whisper_large_v3_turbo() -> Self {
        Self {
            name: "Whisper large-v3-turbo Q5_0 · Portuguese (pt)".into(),
            file_name: "ggml-large-v3-turbo-q5_0.bin".into(),
            artifact: Artifact {
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3-turbo-q5_0.bin".into(),
                sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2".into(),
                bytes: 574_041_195,
            },
        }
    }

    /// Nemotron 3.5 ASR Streaming 0.6B, `q8_0` GGUF, at revision `1c8deaec`.
    /// The digest is the Hugging Face LFS object id and matched the file the
    /// runtime verified on 2026-10-23.
    pub fn pinned() -> Self {
        Self {
            name: "Nemotron 3.5 ASR Streaming 0.6B".into(),
            file_name: "nemotron-3.5-asr-streaming-0.6b.q8_0.gguf".into(),
            artifact: Artifact {
                url: "https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/resolve/1c8deaecc64b91f034d73e08dd8b64625eb3395d/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf".into(),
                sha256: "a5c435f294eea8f88ce68dd27b8c3bfea7f777cb2fbba04fcd30eaa555f429ae".into(),
                bytes: 741_548_352,
            },
        }
    }
}

/// What a download is fetching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStage {
    Runtime,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupProgress {
    pub stage: SetupStage,
    pub received: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub platform_supported: bool,
    pub runtime_ready: bool,
    pub model_ready: bool,
}

impl RuntimeStatus {
    pub fn ready(&self) -> bool {
        self.platform_supported && self.runtime_ready && self.model_ready
    }
}

/// What the runtime heard.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub languages: Vec<String>,
    pub duration_secs: f32,
}

#[derive(Debug, Deserialize)]
struct TranscriptJson {
    text: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    duration: f32,
}

#[derive(Debug, Deserialize)]
struct ErrorJson {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: String,
}

/// Upper bound for one transcription process, model load included.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);
/// A download that receives nothing for this long, headers included, fails.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Keeps a console window from flashing when the GUI starts the CLI.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The runtime and model under one directory HVC owns. The directory must be
/// short: the runtime nests paths and Windows limits them to 260 characters.
#[derive(Debug, Clone)]
pub struct VoiceRuntime {
    root: PathBuf,
    release: RuntimeRelease,
    model: ModelRelease,
    backend: VoiceBackend,
}

impl VoiceRuntime {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_releases(root, RuntimeRelease::pinned(), ModelRelease::pinned())
    }

    /// Other releases, for tests that serve fixtures.
    pub fn with_releases(
        root: impl Into<PathBuf>,
        release: RuntimeRelease,
        model: ModelRelease,
    ) -> Self {
        Self {
            root: root.into(),
            release,
            model,
            backend: VoiceBackend::Nemotron,
        }
    }

    pub fn with_backend(&self, backend: VoiceBackend) -> Self {
        let mut runtime = self.clone();
        runtime.backend = backend;
        if backend != self.backend {
            runtime.release = match backend {
                VoiceBackend::Nemotron => RuntimeRelease::pinned(),
                VoiceBackend::WhisperLargeV3Turbo => RuntimeRelease::whisper_cpp(),
            };
            runtime.model = match backend {
                VoiceBackend::Nemotron => ModelRelease::pinned(),
                VoiceBackend::WhisperLargeV3Turbo => ModelRelease::whisper_large_v3_turbo(),
            };
        }
        runtime
    }

    pub fn version(&self) -> &str {
        match self.backend {
            VoiceBackend::Nemotron => &self.release.version,
            VoiceBackend::WhisperLargeV3Turbo => whisper::VERSION,
        }
    }

    pub fn release(&self) -> &RuntimeRelease {
        &self.release
    }

    pub fn model(&self) -> &ModelRelease {
        &self.model
    }

    fn runtime_dir(&self) -> PathBuf {
        match self.backend {
            VoiceBackend::Nemotron => self.root.join("runtime").join(&self.release.version),
            VoiceBackend::WhisperLargeV3Turbo => self.root.join("whisper").join(whisper::DIRECTORY),
        }
    }

    pub fn executable(&self) -> PathBuf {
        self.runtime_dir().join(self.executable_relative_path())
    }

    fn executable_relative_path(&self) -> PathBuf {
        match self.backend {
            VoiceBackend::Nemotron => PathBuf::from("bin").join(executable_name()),
            VoiceBackend::WhisperLargeV3Turbo => PathBuf::from(if cfg!(windows) {
                "whisper-cli.exe"
            } else {
                "whisper-cli"
            }),
        }
    }

    fn runtime_marker(&self) -> PathBuf {
        self.runtime_dir().join(".hvc-install")
    }

    fn runtime_identity(&self) -> String {
        format!("{} {}", self.release.version, self.release.archive.sha256)
    }

    pub fn model_path(&self) -> PathBuf {
        self.root.join("models").join(&self.model.file_name)
    }

    fn model_marker(&self) -> PathBuf {
        self.root
            .join("models")
            .join(format!("{}.sha256", self.model.file_name))
    }

    fn scratch_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Removes dictated audio and transcript output left by an interrupted process.
    /// Only files named like this runtime's UUID scratch files are touched. Run it before the
    /// first transcription: a file that cannot be removed now is retried on the next start.
    pub fn sweep_scratch(&self) -> ScratchSweep {
        let mut sweep = ScratchSweep::default();
        let Ok(entries) = std::fs::read_dir(self.scratch_dir()) else {
            return sweep;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !entry.file_type().is_ok_and(|kind| kind.is_file())
                || !name.to_str().is_some_and(is_scratch_file_name)
            {
                continue;
            }
            match std::fs::remove_file(entry.path()) {
                Ok(()) => sweep.removed += 1,
                Err(_) => sweep.remaining += 1,
            }
        }
        sweep
    }

    /// Checks files and their install markers; never hashes the model again.
    pub fn status(&self) -> RuntimeStatus {
        let runtime_ready = self.executable().is_file()
            && std::fs::read_to_string(self.runtime_marker())
                .is_ok_and(|marker| marker.trim() == self.runtime_identity());
        let model_ready = std::fs::metadata(self.model_path())
            .is_ok_and(|metadata| metadata.len() == self.model.artifact.bytes)
            && std::fs::read_to_string(self.model_marker())
                .is_ok_and(|marker| marker.trim() == self.model.artifact.sha256);
        RuntimeStatus {
            platform_supported: cfg!(windows),
            runtime_ready,
            model_ready,
        }
    }

    /// Downloads, verifies and unpacks the runtime, then checks that it runs.
    /// Does nothing when the pinned release is already installed.
    pub async fn install_runtime(
        &self,
        progress: &(dyn Fn(SetupProgress) + Send + Sync),
        cancellation: &CancellationToken,
    ) -> Result<(), VoiceError> {
        if !cfg!(windows) {
            return Err(VoiceError::Unsupported);
        }
        if self.status().runtime_ready {
            return Ok(());
        }
        let downloads = self.root.join("downloads");
        tokio::fs::create_dir_all(&downloads).await?;
        let archive = downloads.join(format!("{}.zip", self.release.archive_root));
        download(
            &self.release.archive,
            &archive,
            SetupStage::Runtime,
            progress,
            cancellation,
        )
        .await?;
        let staging = self
            .root
            .join("runtime")
            .join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
        let installed = self
            .unpack_and_check(&archive, &staging, cancellation)
            .await;
        let _ = tokio::fs::remove_file(&archive).await;
        if let Err(error) = installed {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(error);
        }
        Ok(())
    }

    async fn unpack_and_check(
        &self,
        archive: &Path,
        staging: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), VoiceError> {
        tokio::fs::create_dir_all(staging).await?;
        let tar = system_tar();
        let mut command = tokio::process::Command::new(&tar);
        command
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(staging)
            .kill_on_drop(true);
        hide_console(&mut command);
        let output = cancellable(cancellation, command.output())
            .await?
            .map_err(|error| {
                VoiceError::Runtime(format!("could not run {}: {error}", tar.display()))
            })?;
        if !output.status.success() {
            return Err(VoiceError::Runtime(format!(
                "the runtime archive could not be unpacked: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let unpacked = staging.join(&self.release.archive_root);
        let executable = unpacked.join(self.executable_relative_path());
        if !executable.is_file() {
            return Err(VoiceError::Runtime(format!(
                "the runtime archive does not contain {}",
                self.executable_relative_path().display()
            )));
        }
        let version = run_checked(&executable, &["--version"], cancellation).await?;
        if !version.contains(&self.release.version) {
            return Err(VoiceError::Runtime(format!(
                "the runtime reports `{}`, expected version {}",
                version.trim(),
                self.release.version
            )));
        }
        if self.backend == VoiceBackend::Nemotron {
            run_checked(&executable, &["--json", "doctor"], cancellation).await?;
        }
        // Past this point the install replaces the previous runtime: never after a cancel.
        if cancellation.is_cancelled() {
            return Err(VoiceError::Cancelled);
        }
        tokio::fs::write(unpacked.join(".hvc-install"), self.runtime_identity()).await?;
        let target = self.runtime_dir();
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if tokio::fs::metadata(&target).await.is_ok() {
            tokio::fs::remove_dir_all(&target).await?;
        }
        tokio::fs::rename(&unpacked, &target).await?;
        let _ = tokio::fs::remove_dir_all(staging).await;
        Ok(())
    }

    /// Downloads and verifies the model. Does nothing when it is present.
    pub async fn download_model(
        &self,
        progress: &(dyn Fn(SetupProgress) + Send + Sync),
        cancellation: &CancellationToken,
    ) -> Result<(), VoiceError> {
        if self.status().model_ready {
            return Ok(());
        }
        let models = self.root.join("models");
        tokio::fs::create_dir_all(&models).await?;
        download(
            &self.model.artifact,
            &self.model_path(),
            SetupStage::Model,
            progress,
            cancellation,
        )
        .await?;
        tokio::fs::write(self.model_marker(), &self.model.artifact.sha256).await?;
        Ok(())
    }

    /// Transcribes a WAV image with the selected recognizer. Nemotron detects
    /// the language; Whisper uses Portuguese. Audio exists on disk only for the call.
    pub async fn transcribe(
        &self,
        wav: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Transcript, VoiceError> {
        let status = self.status();
        if !status.platform_supported {
            return Err(VoiceError::Unsupported);
        }
        if !status.runtime_ready || !status.model_ready {
            return Err(VoiceError::NotInstalled);
        }
        let audio = ScratchAudio::write(&self.scratch_dir(), wav).await?;
        if self.backend == VoiceBackend::WhisperLargeV3Turbo {
            let duration_secs = whisper::pcm_duration(wav)?;
            return whisper::transcribe(
                &self.executable(),
                &self.model_path(),
                audio.path(),
                duration_secs,
                cancellation,
            )
            .await;
        }
        self.run_transcription(audio.path(), cancellation).await
    }

    async fn run_transcription(
        &self,
        audio: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Transcript, VoiceError> {
        let mut command = tokio::process::Command::new(self.executable());
        command
            .args(["--json", "--quiet", "transcribe"])
            .arg(audio)
            .arg("--model")
            .arg(self.model_path())
            .args(["--device", "cpu", "-f", "json"])
            .env("NEMO_SPEECH_MODEL_DIR", self.root.join("models"))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        hide_console(&mut command);
        let output = tokio::select! {
            output = tokio::time::timeout(TRANSCRIBE_TIMEOUT, command.output()) => output
                .map_err(|_| VoiceError::Runtime("transcription took longer than two minutes".into()))?
                .map_err(|error| VoiceError::Runtime(format!("could not start the runtime: {error}")))?,
            _ = cancellation.cancelled() => return Err(VoiceError::Cancelled),
        };
        parse_transcription(output.status.success(), &output.stdout, &output.stderr)
    }
}

/// Reads the runtime's JSON result, or its JSON error.
pub fn parse_transcription(
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<Transcript, VoiceError> {
    if !success {
        let message = serde_json::from_slice::<ErrorJson>(stderr)
            .map(|error| error.error.message)
            .unwrap_or_else(|_| String::from_utf8_lossy(stderr).trim().to_string());
        return Err(VoiceError::Runtime(format!(
            "transcription failed: {message}"
        )));
    }
    let parsed: TranscriptJson = serde_json::from_slice(stdout).map_err(|error| {
        VoiceError::Runtime(format!("unreadable transcription output: {error}"))
    })?;
    Ok(Transcript {
        text: parsed.text.trim().to_string(),
        languages: parsed.languages,
        duration_secs: parsed.duration,
    })
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "nemo-speech.exe"
    } else {
        "nemo-speech"
    }
}

/// The operating system's own `tar`, by absolute path on Windows so a binary
/// earlier on the PATH cannot stand in for it.
pub(crate) fn system_tar() -> PathBuf {
    if cfg!(windows) {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(system_root).join("System32").join("tar.exe")
    } else {
        PathBuf::from("tar")
    }
}

pub(crate) fn hide_console(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

async fn run_checked(
    executable: &Path,
    args: &[&str],
    cancellation: &CancellationToken,
) -> Result<String, VoiceError> {
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);
    let output = cancellable(
        cancellation,
        tokio::time::timeout(Duration::from_secs(60), command.output()),
    )
    .await?
    .map_err(|_| VoiceError::Runtime(format!("`{}` did not answer", args.join(" "))))?
    .map_err(|error| VoiceError::Runtime(format!("could not run the runtime: {error}")))?;
    if !output.status.success() {
        return Err(VoiceError::Runtime(format!(
            "`voice runtime {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs `work` until it finishes or `cancellation` fires. A cancel drops `work`, which kills a
/// child process started with `kill_on_drop`.
async fn cancellable<T>(
    cancellation: &CancellationToken,
    work: impl std::future::Future<Output = T>,
) -> Result<T, VoiceError> {
    tokio::select! {
        output = work => Ok(output),
        _ = cancellation.cancelled() => Err(VoiceError::Cancelled),
    }
}

/// What [`VoiceRuntime::sweep_scratch`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScratchSweep {
    pub removed: usize,
    /// Files that could not be removed now (for example still open); retried next start.
    pub remaining: usize,
}

/// One dictated WAV in the scratch folder. Dropping it removes the file, so the audio goes
/// away when the transcription ends however it ends: success, error, timeout, cancel or a
/// dropped future. A file a dying process still holds, or one a crash leaves, is removed by
/// [`VoiceRuntime::sweep_scratch`] on the next start.
struct ScratchAudio(PathBuf);

impl ScratchAudio {
    async fn write(scratch: &Path, wav: &[u8]) -> Result<Self, VoiceError> {
        tokio::fs::create_dir_all(scratch).await?;
        // Owned before the first byte, so a partial write is removed too.
        let audio = Self(scratch.join(format!("{}.wav", uuid::Uuid::new_v4().simple())));
        tokio::fs::write(&audio.0, wav).await?;
        Ok(audio)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchAudio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// UUID names owned by the recorder or by its native transcription output guard.
fn is_scratch_file_name(name: &str) -> bool {
    name.strip_suffix(".wav")
        .or_else(|| name.strip_suffix(".json"))
        .is_some_and(|stem| {
            stem.len() == 32
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// Streams `artifact` into `target`, hashing as it goes. The file appears at
/// `target` only after its size and digest match; a partial file is removed.
pub(crate) async fn download(
    artifact: &Artifact,
    target: &Path,
    stage: SetupStage,
    progress: &(dyn Fn(SetupProgress) + Send + Sync),
    cancellation: &CancellationToken,
) -> Result<(), VoiceError> {
    let partial = target.with_extension("part");
    let result = fetch(artifact, &partial, stage, progress, cancellation).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
        return result;
    }
    tokio::fs::rename(&partial, target).await?;
    Ok(())
}

async fn fetch(
    artifact: &Artifact,
    partial: &Path,
    stage: SetupStage,
    progress: &(dyn Fn(SetupProgress) + Send + Sync),
    cancellation: &CancellationToken,
) -> Result<(), VoiceError> {
    let client = reqwest::Client::builder()
        .https_only(artifact.url.starts_with("https://"))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(STALL_TIMEOUT)
        .user_agent(concat!("hard-vibe-code/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| VoiceError::Network(error.to_string()))?;
    let response = cancellable(cancellation, client.get(&artifact.url).send())
        .await?
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| VoiceError::Network(error.to_string()))?;
    let mut file = tokio::fs::File::create(partial).await?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut reported = 0u64;
    let mut stream = response.bytes_stream();
    progress(SetupProgress {
        stage,
        received: 0,
        total: artifact.bytes,
    });
    loop {
        let chunk = tokio::select! {
            chunk = stream.next() => chunk,
            _ = cancellation.cancelled() => return Err(VoiceError::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|error| VoiceError::Network(error.to_string()))?;
        received += chunk.len() as u64;
        if received > artifact.bytes {
            return Err(VoiceError::Integrity(format!(
                "the download is larger than the expected {} bytes",
                artifact.bytes
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        // About one update per 1% keeps the event bus quiet on a fast link.
        if received == artifact.bytes || received - reported >= artifact.bytes / 100 {
            reported = received;
            progress(SetupProgress {
                stage,
                received,
                total: artifact.bytes,
            });
        }
    }
    file.flush().await?;
    drop(file);
    if received != artifact.bytes {
        return Err(VoiceError::Integrity(format!(
            "received {received} bytes, expected {}",
            artifact.bytes
        )));
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if digest != artifact.sha256 {
        return Err(VoiceError::Integrity(format!(
            "SHA-256 {digest} does not match the pinned {}",
            artifact.sha256
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Serves `body` once per connection, `connections` times.
    fn serve(body: Vec<u8>, connections: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let header = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{address}/artifact")
    }

    fn model(url: String, body: &[u8], sha: Option<String>) -> ModelRelease {
        ModelRelease {
            name: "fixture".into(),
            file_name: "fixture.gguf".into(),
            artifact: Artifact {
                url,
                sha256: sha.unwrap_or_else(|| sha256(body)),
                bytes: body.len() as u64,
            },
        }
    }

    #[tokio::test]
    async fn a_verified_model_download_lands_with_its_marker_and_progress() {
        let root = tempfile::tempdir().unwrap();
        let body = vec![7u8; 300_000];
        let runtime = VoiceRuntime::with_releases(
            root.path(),
            RuntimeRelease::pinned(),
            model(serve(body.clone(), 1), &body, None),
        );
        assert!(!runtime.status().model_ready);
        let seen = std::sync::Mutex::new(Vec::new());
        runtime
            .download_model(
                &|p| seen.lock().unwrap().push(p.received),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(runtime.status().model_ready);
        assert_eq!(std::fs::read(runtime.model_path()).unwrap(), body);
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.first(), Some(&0));
        assert_eq!(seen.last(), Some(&300_000));
        assert!(seen.len() <= 102, "about one update per percent");
        // Present and marked: a second call fetches nothing.
        runtime
            .download_model(&|_| panic!("no second download"), &CancellationToken::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_digest_mismatch_leaves_nothing_behind() {
        let root = tempfile::tempdir().unwrap();
        let body = vec![1u8; 10_000];
        let runtime = VoiceRuntime::with_releases(
            root.path(),
            RuntimeRelease::pinned(),
            model(serve(body.clone(), 1), &body, Some("0".repeat(64))),
        );
        let error = runtime
            .download_model(&|_| {}, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, VoiceError::Integrity(_)), "{error}");
        assert!(!runtime.model_path().exists());
        assert!(!runtime.model_path().with_extension("part").exists());
        assert!(!runtime.status().model_ready);
    }

    #[tokio::test]
    async fn a_size_mismatch_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let body = vec![2u8; 5_000];
        let mut release = model(serve(body.clone(), 1), &body, None);
        release.artifact.bytes = 4_000;
        let runtime = VoiceRuntime::with_releases(root.path(), RuntimeRelease::pinned(), release);
        let error = runtime
            .download_model(&|_| {}, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, VoiceError::Integrity(_)), "{error}");
        assert!(!runtime.model_path().exists());
    }

    /// Accepts one connection, reads the request, sends `reply` and then goes silent.
    fn serve_then_stall(reply: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(&reply);
                std::thread::sleep(Duration::from_secs(30));
            }
        });
        format!("http://{address}/artifact")
    }

    fn cancel_after(delay: Duration) -> CancellationToken {
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            cancel.cancel();
        });
        token
    }

    #[tokio::test]
    async fn cancelling_while_the_server_withholds_headers_ends_the_download() {
        let root = tempfile::tempdir().unwrap();
        let body = vec![3u8; 10_000];
        let runtime = VoiceRuntime::with_releases(
            root.path(),
            RuntimeRelease::pinned(),
            model(serve_then_stall(Vec::new()), &body, None),
        );
        let token = cancel_after(Duration::from_millis(200));
        let started = std::time::Instant::now();

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            runtime.download_model(&|_| {}, &token),
        )
        .await
        .expect("a cancel ends the wait for headers")
        .unwrap_err();

        assert!(matches!(error, VoiceError::Cancelled), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!runtime.model_path().exists());
        assert!(!runtime.model_path().with_extension("part").exists());
        assert!(!runtime.status().model_ready);
    }

    #[tokio::test]
    async fn cancelling_a_stalled_body_removes_the_partial_file() {
        let root = tempfile::tempdir().unwrap();
        let body = vec![4u8; 10_000];
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(&body[..100]);
        let runtime = VoiceRuntime::with_releases(
            root.path(),
            RuntimeRelease::pinned(),
            model(serve_then_stall(reply), &body, None),
        );
        let token = cancel_after(Duration::from_millis(300));

        let error = tokio::time::timeout(
            Duration::from_secs(10),
            runtime.download_model(&|_| {}, &token),
        )
        .await
        .expect("a cancel ends a stalled body")
        .unwrap_err();

        assert!(matches!(error, VoiceError::Cancelled), "{error}");
        assert!(!runtime.model_path().exists());
        assert!(!runtime.model_path().with_extension("part").exists());
        assert!(!runtime.status().model_ready);
    }

    #[tokio::test]
    async fn a_slow_runtime_check_ends_when_setup_is_cancelled() {
        let (program, args): (PathBuf, &[&str]) = if cfg!(windows) {
            let system_root =
                std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            (
                PathBuf::from(system_root).join("System32").join("PING.EXE"),
                &["-n", "30", "127.0.0.1"],
            )
        } else {
            (PathBuf::from("sleep"), &["30"])
        };
        let token = cancel_after(Duration::from_millis(200));
        let started = std::time::Instant::now();

        let error = run_checked(&program, args, &token).await.unwrap_err();

        assert!(matches!(error, VoiceError::Cancelled), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn scratch_audio_is_removed_however_the_transcription_ends() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("tmp");

        let audio = ScratchAudio::write(&scratch, b"RIFF").await.unwrap();
        let path = audio.path().to_path_buf();
        assert!(is_scratch_file_name(
            path.file_name().unwrap().to_str().unwrap()
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"RIFF");
        drop(audio);
        assert!(!path.exists(), "removed after a normal end");

        let failed: Result<(), VoiceError> = async {
            let _audio = ScratchAudio::write(&scratch, b"RIFF").await?;
            Err(VoiceError::Runtime("transcription failed".into()))
        }
        .await;
        assert!(failed.is_err());

        let abandoned = tokio::time::timeout(Duration::from_millis(50), async {
            let _audio = ScratchAudio::write(&scratch, b"RIFF").await.unwrap();
            std::future::pending::<()>().await;
        })
        .await;
        assert!(abandoned.is_err(), "the caller dropped the future");

        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }

    #[test]
    fn the_startup_sweep_removes_only_leftover_scratch_recordings() {
        let root = tempfile::tempdir().unwrap();
        let runtime = VoiceRuntime::new(root.path());
        assert_eq!(
            runtime.sweep_scratch(),
            ScratchSweep::default(),
            "no folder yet"
        );

        let scratch = root.path().join("tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let leftover = scratch.join(format!("{}.wav", uuid::Uuid::new_v4().simple()));
        std::fs::write(&leftover, b"RIFF").unwrap();
        let kept = [
            "notes.wav",
            "0123456789ABCDEF0123456789ABCDEF.wav",
            "0123456789abcdef0123456789abcdef.txt",
            "0123456789abcdef0123456789abcde.wav",
        ];
        for name in kept {
            std::fs::write(scratch.join(name), b"keep").unwrap();
        }
        std::fs::create_dir(scratch.join("fedcba9876543210fedcba9876543210.wav")).unwrap();

        let sweep = runtime.sweep_scratch();

        assert_eq!(
            sweep,
            ScratchSweep {
                removed: 1,
                remaining: 0
            }
        );
        assert!(!leftover.exists());
        for name in kept {
            assert!(
                scratch.join(name).exists(),
                "{name} is not a scratch recording"
            );
        }
        assert!(
            scratch
                .join("fedcba9876543210fedcba9876543210.wav")
                .is_dir()
        );
    }

    #[test]
    fn a_model_without_its_marker_or_with_another_size_is_not_ready() {
        let root = tempfile::tempdir().unwrap();
        let runtime = VoiceRuntime::with_releases(
            root.path(),
            RuntimeRelease::pinned(),
            model("http://unused".into(), b"abc", None),
        );
        std::fs::create_dir_all(root.path().join("models")).unwrap();
        std::fs::write(runtime.model_path(), b"abc").unwrap();
        assert!(!runtime.status().model_ready, "no marker");
        std::fs::write(runtime.model_marker(), sha256(b"abc")).unwrap();
        assert!(runtime.status().model_ready);
        std::fs::write(runtime.model_path(), b"abcd").unwrap();
        assert!(!runtime.status().model_ready, "size changed");
    }

    #[test]
    fn transcription_output_and_errors_are_read_from_the_runtime_json() {
        let ok = br#"{"file":"x.wav","text":" O bot\u00e3o some. ","confidence":1,"duration":12.5,"languages":["pt-PT"],"words":[]}"#;
        let transcript = parse_transcription(true, ok, b"").unwrap();
        assert_eq!(transcript.text, "O bot\u{e3}o some.");
        assert_eq!(transcript.languages, ["pt-PT"]);
        assert!((transcript.duration_secs - 12.5).abs() < f32::EPSILON);

        let silence =
            br#"{"file":"x.wav","text":"","confidence":1,"duration":1,"languages":[],"words":[]}"#;
        assert_eq!(parse_transcription(true, silence, b"").unwrap().text, "");

        let failure = br#"{"error":{"message":"x.wav is not a file","type":"invalid_argument","command":"transcribe","exit_code":2}}"#;
        let error = parse_transcription(false, b"", failure).unwrap_err();
        assert!(error.to_string().contains("x.wav is not a file"), "{error}");
        assert!(parse_transcription(true, b"not json", b"").is_err());
    }

    #[test]
    fn the_pinned_releases_are_https_and_complete() {
        let runtime = RuntimeRelease::pinned();
        let model = ModelRelease::pinned();
        for artifact in [&runtime.archive, &model.artifact] {
            assert!(artifact.url.starts_with("https://"));
            assert_eq!(artifact.sha256.len(), 64);
            assert!(
                artifact
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
            assert!(artifact.bytes > 0);
        }
        assert!(runtime.archive.url.contains(&runtime.archive_root));
        assert!(model.artifact.url.ends_with(&model.file_name));
    }

    #[test]
    fn native_whisper_requires_its_own_runtime_and_migrated_model() {
        let root = tempfile::tempdir().unwrap();
        let nemotron = VoiceRuntime::new(root.path());
        let whisper = nemotron.with_backend(VoiceBackend::WhisperLargeV3Turbo);
        assert_eq!(whisper.version(), "1.9.5");
        assert_eq!(whisper.model().file_name, "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(whisper.model().artifact.bytes, 574_041_195);
        assert_ne!(nemotron.executable(), whisper.executable());
        assert!(!whisper.status().runtime_ready);

        std::fs::create_dir_all(whisper.executable().parent().unwrap()).unwrap();
        std::fs::write(whisper.executable(), b"fixture executable").unwrap();
        std::fs::write(whisper.runtime_marker(), "old Python runtime").unwrap();
        assert!(!whisper.status().runtime_ready);
        std::fs::write(whisper.runtime_marker(), whisper.runtime_identity()).unwrap();
        assert!(whisper.status().runtime_ready);
        assert!(
            !whisper.status().model_ready,
            "medium.pt cannot satisfy turbo readiness"
        );
        assert!(!nemotron.status().runtime_ready);
    }

    #[test]
    fn native_whisper_artifacts_are_pinned_to_verified_native_sources() {
        let runtime = RuntimeRelease::whisper_cpp();
        let model = ModelRelease::whisper_large_v3_turbo();
        assert_eq!(runtime.version, whisper::VERSION);
        assert_eq!(runtime.archive_root, "Release");
        assert!(runtime.archive.url.ends_with("/b5454/whisper-bin-x64.zip"));
        assert_eq!(runtime.archive.bytes, 8_928_640);
        assert_eq!(model.artifact.bytes, 574_041_195);
        for artifact in [&runtime.archive, &model.artifact] {
            assert!(artifact.url.starts_with("https://"));
            assert_eq!(artifact.sha256.len(), 64);
            assert!(
                artifact
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
        }
        assert!(
            model
                .artifact
                .url
                .contains("/5359861c739e955e79d9a303bcbc70fb988958b1/")
        );
        assert!(model.artifact.url.ends_with(&model.file_name));
    }

    #[test]
    fn a_crash_left_transcript_is_swept_without_touching_other_json() {
        let root = tempfile::tempdir().unwrap();
        let runtime = VoiceRuntime::new(root.path());
        let scratch = root.path().join("tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let transcript = scratch.join("0123456789abcdef0123456789abcdef.json");
        std::fs::write(&transcript, b"private transcript").unwrap();
        let kept = scratch.join("settings.json");
        std::fs::write(&kept, b"keep").unwrap();
        assert_eq!(runtime.sweep_scratch().removed, 1);
        assert!(!transcript.exists());
        assert!(kept.exists());
    }
}
