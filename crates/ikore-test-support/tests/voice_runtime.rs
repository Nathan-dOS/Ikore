//! ADR-029 voice runtime, end to end against a fixture runtime: Ikore fetches a
//! release archive and a model over HTTP, refuses anything whose pinned digest
//! does not match, unpacks the runtime with the system `tar`, checks that it
//! runs, and transcribes WAV images through it.
#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    reason = "Test-only fixture helpers fail immediately on invalid isolated setup"
)]

use ikore_voice::runtime::{Artifact, ModelRelease, RuntimeRelease};
use ikore_voice::{VoiceError, VoiceRuntime};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use tokio_util::sync::CancellationToken;

const ROOT: &str = "nemo-speech-0.2.0-windows-x86_64-cpu";

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Serves `body` to every connection until the test ends.
fn serve(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://{address}/download")
}

/// A release zip shaped like NVIDIA's: one top-level folder holding `bin/`.
fn runtime_archive(scratch: &Path) -> Vec<u8> {
    let bin = scratch.join("archive").join(ROOT).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_nemo_fixture"),
        bin.join("nemo-speech.exe"),
    )
    .unwrap();
    let zip = scratch.join("runtime.zip");
    let tar = Path::new(&std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("tar.exe");
    let status = std::process::Command::new(tar)
        .args(["-a", "-c", "-f"])
        .arg(&zip)
        .arg("-C")
        .arg(scratch.join("archive"))
        .arg(ROOT)
        .status()
        .unwrap();
    assert!(status.success(), "the fixture archive could not be built");
    std::fs::read(zip).unwrap()
}

fn release(url: String, archive: &[u8], sha256: String) -> RuntimeRelease {
    RuntimeRelease {
        version: "0.2.0".into(),
        archive: Artifact {
            url,
            sha256,
            bytes: archive.len() as u64,
        },
        archive_root: ROOT.into(),
    }
}

fn model(url: String, body: &[u8]) -> ModelRelease {
    ModelRelease {
        name: "Fixture model".into(),
        file_name: "fixture.q8_0.gguf".into(),
        artifact: Artifact {
            url,
            sha256: sha256(body),
            bytes: body.len() as u64,
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ikore_installs_the_runtime_downloads_the_model_and_transcribes() {
    let scratch = tempfile::tempdir().unwrap();
    let archive = runtime_archive(scratch.path());
    let model_body = vec![42u8; 64 * 1024];
    let voice_root = scratch.path().join("v");
    let runtime = VoiceRuntime::with_releases(
        &voice_root,
        release(serve(archive.clone()), &archive, sha256(&archive)),
        model(serve(model_body.clone()), &model_body),
    );
    assert!(!runtime.status().runtime_ready && !runtime.status().model_ready);
    let none = CancellationToken::new();
    let silence = ikore_voice::wav::encode_mono_16(&[0; 16_000], 16_000);
    assert!(matches!(
        runtime.transcribe(&silence, &none).await,
        Err(VoiceError::NotInstalled)
    ));

    runtime.install_runtime(&|_| {}, &none).await.unwrap();
    assert!(runtime.status().runtime_ready);
    assert!(runtime.executable().is_file());
    assert!(
        !voice_root
            .join("downloads")
            .join(format!("{ROOT}.zip"))
            .exists(),
        "the archive is removed once unpacked"
    );
    runtime.download_model(&|_| {}, &none).await.unwrap();
    assert!(runtime.status().ready());

    let speech: Vec<i16> = (0..16_000)
        .map(|i| ((i % 40) * 400 - 8_000) as i16)
        .collect();
    let heard = runtime
        .transcribe(&ikore_voice::wav::encode_mono_16(&speech, 16_000), &none)
        .await
        .unwrap();
    assert_eq!(heard.text, "Fixture transcript of the note.");
    assert_eq!(heard.languages, ["pt-BR"]);
    assert_eq!(runtime.transcribe(&silence, &none).await.unwrap().text, "");
    let scratch_audio = std::fs::read_dir(voice_root.join("tmp")).unwrap().count();
    assert_eq!(scratch_audio, 0, "no audio is kept after transcription");

    // Installed: a second install fetches nothing.
    runtime
        .install_runtime(&|_| panic!("no second download"), &none)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_archive_with_another_digest_is_never_unpacked() {
    let scratch = tempfile::tempdir().unwrap();
    let archive = runtime_archive(scratch.path());
    let voice_root = scratch.path().join("v");
    let runtime = VoiceRuntime::with_releases(
        &voice_root,
        release(serve(archive.clone()), &archive, "f".repeat(64)),
        model("http://127.0.0.1:9/unused".into(), b"x"),
    );
    let error = runtime
        .install_runtime(&|_| {}, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, VoiceError::Integrity(_)), "{error}");
    assert!(!runtime.executable().exists());
    assert!(!runtime.status().runtime_ready);
    let leftovers = std::fs::read_dir(voice_root.join("downloads"))
        .unwrap()
        .count();
    assert_eq!(leftovers, 0, "the rejected download is deleted");
}

/// The pinned NVIDIA runtime and model, for a manual check on a real machine:
/// `IKORE_VOICE_LIVE_DIR=C:\voice IKORE_VOICE_LIVE_WAV=note.wav cargo test --test
/// voice_runtime -- --ignored`. Downloads about 712 MB on first use.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads the real runtime and model"]
async fn live_pinned_runtime_transcribes_a_recording() {
    let (Ok(dir), Ok(wav)) = (
        std::env::var("IKORE_VOICE_LIVE_DIR").or_else(|_| std::env::var("HVC_VOICE_LIVE_DIR")),
        std::env::var("IKORE_VOICE_LIVE_WAV").or_else(|_| std::env::var("HVC_VOICE_LIVE_WAV")),
    ) else {
        panic!(
            "set IKORE_VOICE_LIVE_DIR (or HVC_VOICE_LIVE_DIR) and IKORE_VOICE_LIVE_WAV (or HVC_VOICE_LIVE_WAV)"
        );
    };
    let runtime = VoiceRuntime::new(dir);
    let none = CancellationToken::new();
    runtime.install_runtime(&|_| {}, &none).await.unwrap();
    runtime.download_model(&|_| {}, &none).await.unwrap();
    let transcript = runtime
        .transcribe(&std::fs::read(wav).unwrap(), &none)
        .await
        .unwrap();
    println!("{transcript:?}");
    assert!(!transcript.text.is_empty());
}
