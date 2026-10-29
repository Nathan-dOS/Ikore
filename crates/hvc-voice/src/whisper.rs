//! Native whisper.cpp CPU transcription; no Python, `PyTorch` or `FFmpeg` runtime.
use crate::VoiceError;
use crate::runtime::{Transcript, hide_console};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const VERSION: &str = "1.9.5";
pub(crate) const DIRECTORY: &str = "whisper-cpp-1.9.5-cpu";
const MAX_JSON_BYTES: u64 = 2 * 1024 * 1024;

/// One temporary transcript next to the host's UUID-named scratch WAV.
/// A crash-left output is also removed by the voice runtime's startup sweep.
struct ScratchOutput {
    base: PathBuf,
    json: PathBuf,
}

impl ScratchOutput {
    fn for_audio(audio: &Path) -> Self {
        Self {
            base: audio.with_extension(""),
            json: audio.with_extension("json"),
        }
    }

    fn base(&self) -> &Path {
        &self.base
    }

    fn path(&self) -> &Path {
        &self.json
    }
}

impl Drop for ScratchOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.json);
    }
}

fn transcription_command(
    executable: &Path,
    model: &Path,
    audio: &Path,
    output_base: &Path,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(executable);
    let threads = std::thread::available_parallelism().map_or(2, |count| count.get().min(4));
    command
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(audio)
        .args(["-l", "pt", "-oj", "-of"])
        .arg(output_base)
        .args(["-ng", "-nt", "-t"])
        .arg(threads.to_string());
    command
}

async fn run(
    command: &mut tokio::process::Command,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, VoiceError> {
    if cancellation.is_cancelled() {
        return Err(VoiceError::Cancelled);
    }
    command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    hide_console(command);
    let output = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(VoiceError::Cancelled),
        result = tokio::time::timeout(timeout, command.output()) => result
            .map_err(|_| VoiceError::Runtime("Whisper timed out; try a shorter recording.".into()))??,
    };
    if !output.status.success() {
        return Err(VoiceError::Runtime(format!(
            "Whisper: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

#[derive(Deserialize)]
struct CliResult {
    result: CliLanguage,
    transcription: Vec<CliSegment>,
}

#[derive(Deserialize)]
struct CliLanguage {
    language: String,
}

#[derive(Deserialize)]
struct CliSegment {
    text: String,
    offsets: CliOffsets,
}

#[derive(Deserialize)]
struct CliOffsets {
    from: u64,
    to: u64,
}

fn parse_cli_json(json: &[u8], duration_secs: f32) -> Result<Transcript, VoiceError> {
    let result: CliResult = serde_json::from_slice(json)
        .map_err(|error| VoiceError::Runtime(format!("unreadable Whisper output: {error}")))?;
    if result.result.language.trim().is_empty() {
        return Err(VoiceError::Runtime("Whisper returned no language.".into()));
    }
    let mut text = String::new();
    for segment in result.transcription {
        if segment.offsets.to < segment.offsets.from {
            return Err(VoiceError::Runtime(
                "Whisper returned invalid segment offsets.".into(),
            ));
        }
        text.push_str(&segment.text);
    }
    Ok(Transcript {
        text: text.trim().to_string(),
        languages: vec![result.result.language],
        duration_secs,
    })
}

/// Reads the recorder's PCM WAV duration rather than Whisper's padded segment end.
pub(crate) fn pcm_duration(wav: &[u8]) -> Result<f32, VoiceError> {
    let invalid =
        || VoiceError::Runtime("Whisper expects a complete mono 16-bit PCM WAV at 16 kHz.".into());
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(invalid());
    }
    let riff_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]) as usize;
    let limit = riff_size.checked_add(8).ok_or_else(invalid)?;
    if limit > wav.len() {
        return Err(invalid());
    }
    let mut position = 12usize;
    let mut format_valid = false;
    let mut data_bytes = None;
    while position.checked_add(8).is_some_and(|end| end <= limit) {
        let size = u32::from_le_bytes([
            wav[position + 4],
            wav[position + 5],
            wav[position + 6],
            wav[position + 7],
        ]) as usize;
        let payload = position + 8;
        let end = payload.checked_add(size).ok_or_else(invalid)?;
        if end > limit {
            return Err(invalid());
        }
        let chunk = &wav[payload..end];
        match &wav[position..position + 4] {
            b"fmt " => {
                if chunk.len() < 16 {
                    return Err(invalid());
                }
                let encoding = u16::from_le_bytes([chunk[0], chunk[1]]);
                let channels = u16::from_le_bytes([chunk[2], chunk[3]]);
                let rate = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                let align = u16::from_le_bytes([chunk[12], chunk[13]]);
                let bits = u16::from_le_bytes([chunk[14], chunk[15]]);
                format_valid = (encoding, channels, rate, align, bits) == (1, 1, 16_000, 2, 16);
                if !format_valid {
                    return Err(invalid());
                }
            }
            b"data" => {
                if !size.is_multiple_of(2) || data_bytes.is_some() {
                    return Err(invalid());
                }
                data_bytes = Some(size);
            }
            _ => {}
        }
        position = end.checked_add(size % 2).ok_or_else(invalid)?;
    }
    if !format_valid || position != limit {
        return Err(invalid());
    }
    let data_bytes = data_bytes.ok_or_else(invalid)?;
    Ok(crate::wav::duration_secs(data_bytes / 2, 16_000))
}

async fn read_transcript(path: &Path, duration_secs: f32) -> Result<Transcript, VoiceError> {
    let metadata = tokio::fs::metadata(path).await.map_err(|error| {
        VoiceError::Runtime(format!("Whisper did not write its transcription: {error}"))
    })?;
    if metadata.len() > MAX_JSON_BYTES {
        return Err(VoiceError::Runtime(
            "Whisper transcription exceeded the output limit.".into(),
        ));
    }
    let json = tokio::fs::read(path).await?;
    parse_cli_json(&json, duration_secs)
}

pub(crate) async fn transcribe(
    executable: &Path,
    model: &Path,
    audio: &Path,
    duration_secs: f32,
    cancellation: &CancellationToken,
) -> Result<Transcript, VoiceError> {
    let output = ScratchOutput::for_audio(audio);
    let mut command = transcription_command(executable, model, audio, output.base());
    run(&mut command, Duration::from_secs(600), cancellation).await?;
    read_transcript(output.path(), duration_secs).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_segments_keep_portuguese_text_and_do_not_count_the_thirty_second_padding() {
        let json = br#"{"systeminfo":"cpu","model":{"type":"large v3 turbo"},"params":{"language":"pt"},"result":{"language":"pt"},"transcription":[{"timestamps":{"from":"00:00:00,000","to":"00:00:01,250"},"offsets":{"from":0,"to":1250},"text":" O bot\u00e3o desapareceu."},{"offsets":{"from":1250,"to":30000},"text":" Quero corrigir a transcri\u00e7\u00e3o. "}]}"#;
        let result = parse_cli_json(json, 2.75).unwrap();
        assert_eq!(
            result.text,
            "O botão desapareceu. Quero corrigir a transcrição."
        );
        assert_eq!(result.languages, ["pt"]);
        assert!((result.duration_secs - 2.75).abs() < f32::EPSILON);
    }

    #[test]
    fn silence_is_valid_but_wrong_or_malformed_cli_output_is_rejected() {
        let silence = br#"{"result":{"language":"pt"},"transcription":[]}"#;
        let result = parse_cli_json(silence, 1.5).unwrap();
        assert!(result.text.is_empty());
        assert_eq!(result.duration_secs, 1.5);
        for invalid in [
            b"not json".as_slice(),
            br#"{"text":"old Python output","languages":["pt"],"duration":1}"#,
            br#"{"result":{"language":"pt"},"transcription":[{"text":"missing offsets"}]}"#,
            br#"{"result":{"language":"pt"},"transcription":[{"offsets":{"from":2000,"to":1000},"text":"backwards"}]}"#,
            br#"{"result":{"language":"pt"},"transcription":[{"offsets":{"from":0,"to":-1},"text":"negative"}]}"#,
        ] {
            assert!(parse_cli_json(invalid, 1.5).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn pcm_duration_accepts_ancillary_chunks_but_rejects_truncated_and_wrong_formats() {
        let wav = crate::wav::encode_mono_16(&vec![0; 44_000], 16_000);
        assert!((pcm_duration(&wav).unwrap() - 2.75).abs() < f32::EPSILON);
        let mut ancillary = wav[..36].to_vec();
        ancillary.extend_from_slice(b"JUNK\x01\x00\x00\x00x\x00");
        ancillary.extend_from_slice(&wav[36..]);
        let riff_bytes = u32::try_from(ancillary.len() - 8).unwrap();
        ancillary[4..8].copy_from_slice(&riff_bytes.to_le_bytes());
        assert_eq!(pcm_duration(&ancillary).unwrap(), 2.75);

        assert!(pcm_duration(b"not a WAV").is_err());
        assert!(pcm_duration(&wav[..wav.len() - 1]).is_err());
        assert!(pcm_duration(&crate::wav::encode_mono_16(&[0; 20], 8_000)).is_err());
        let mut wrong = wav.clone();
        wrong[22..24].copy_from_slice(&2u16.to_le_bytes());
        assert!(pcm_duration(&wrong).is_err());
        let mut missing = wav[..36].to_vec();
        missing[4..8].copy_from_slice(&28u32.to_le_bytes());
        assert!(pcm_duration(&missing).is_err());
    }

    #[tokio::test]
    async fn a_success_exit_without_json_and_an_oversized_output_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("result.json");
        assert!(read_transcript(&path, 1.0).await.is_err());
        std::fs::write(
            &path,
            vec![b' '; usize::try_from(MAX_JSON_BYTES + 1).unwrap()],
        )
        .unwrap();
        let error = read_transcript(&path, 1.0).await.unwrap_err();
        assert!(error.to_string().contains("output limit"), "{error}");
    }

    #[test]
    fn cli_arguments_keep_paths_as_separate_values_and_force_local_portuguese_cpu() {
        let executable = Path::new("voice runtime/whisper-cli.exe");
        let model = Path::new("models with spaces/large-v3-turbo.bin");
        let audio = Path::new("scratch audio/0123456789abcdef0123456789abcdef.wav");
        let output = Path::new("scratch audio/0123456789abcdef0123456789abcdef");
        let command = transcription_command(executable, model, audio, output);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_os_string())
            .collect();
        assert_eq!(command.as_std().get_program(), executable.as_os_str());
        assert_eq!(args[0], "-m");
        assert_eq!(args[1], model.as_os_str());
        assert_eq!(args[2], "-f");
        assert_eq!(args[3], audio.as_os_str());
        assert_eq!(args[4], "-l");
        assert_eq!(args[5], "pt");
        assert_eq!(args[6], "-oj");
        assert_eq!(args[7], "-of");
        assert_eq!(args[8], output.as_os_str());
        assert!(args.iter().any(|arg| arg == "-ng"));
        assert!(args.iter().any(|arg| arg == "-nt"));
        assert!(!args.iter().any(|arg| arg == "-tr"));
    }

    #[tokio::test]
    async fn transcript_output_is_removed_on_success_error_and_dropped_future() {
        let root = tempfile::tempdir().unwrap();
        let audio = root.path().join("0123456789abcdef0123456789abcdef.wav");
        let output = ScratchOutput::for_audio(&audio);
        let json = output.path().to_path_buf();
        assert_eq!(output.base(), audio.with_extension(""));
        std::fs::write(&json, b"private transcript").unwrap();
        drop(output);
        assert!(!json.exists());

        let failed: Result<(), VoiceError> = async {
            let output = ScratchOutput::for_audio(&audio);
            std::fs::write(output.path(), b"partial").unwrap();
            Err(VoiceError::Runtime("child failed".into()))
        }
        .await;
        assert!(failed.is_err());
        assert!(!json.exists());

        let abandoned = tokio::time::timeout(Duration::from_millis(20), async {
            let output = ScratchOutput::for_audio(&audio);
            std::fs::write(output.path(), b"partial").unwrap();
            std::future::pending::<()>().await;
            drop(output);
        })
        .await;
        assert!(abandoned.is_err());
        assert!(!json.exists());
    }

    #[tokio::test]
    async fn cancellation_and_timeout_stop_a_running_native_check() {
        let (program, args): (PathBuf, &[&str]) = if cfg!(windows) {
            let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            (
                PathBuf::from(root).join("System32/PING.EXE"),
                &["-n", "30", "127.0.0.1"],
            )
        } else {
            (PathBuf::from("sleep"), &["30"])
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut command = tokio::process::Command::new(&program);
        command.args(args);
        assert!(matches!(
            run(&mut command, Duration::from_secs(2), &cancel).await,
            Err(VoiceError::Cancelled)
        ));

        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });
        let mut command = tokio::process::Command::new(&program);
        command.args(args);
        let started = std::time::Instant::now();
        assert!(matches!(
            run(&mut command, Duration::from_secs(5), &cancel).await,
            Err(VoiceError::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut command = tokio::process::Command::new(&program);
        command.args(args);
        let error = run(
            &mut command,
            Duration::from_millis(30),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");

        let mut command = tokio::process::Command::new(&program);
        command.arg("--not-a-real-option");
        let error = run(
            &mut command,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Whisper"), "{error}");
    }
}
