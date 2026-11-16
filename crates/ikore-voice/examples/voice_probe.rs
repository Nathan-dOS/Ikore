//! Install or test a local voice backend without recording from the microphone.
use ikore_voice::{VoiceBackend, VoiceRuntime};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .ok_or("usage: voice_probe ROOT nemotron|whisper-large-v3-turbo [AUDIO.wav]")?;
    let backend =
        VoiceBackend::parse(&args.next().ok_or("missing backend")?).ok_or("unknown backend")?;
    let runtime = VoiceRuntime::new(root).with_backend(backend);
    let cancel = CancellationToken::new();
    let progress =
        |p: ikore_voice::SetupProgress| eprintln!("{:?}: {} / {}", p.stage, p.received, p.total);
    runtime.install_runtime(&progress, &cancel).await?;
    runtime.download_model(&progress, &cancel).await?;
    println!("{}: {:?}", backend.label(), runtime.status());
    if let Some(audio) = args.next() {
        let wav = std::fs::read(audio)?;
        let started = std::time::Instant::now();
        let result = runtime.transcribe(&wav, &cancel).await?;
        println!("{result:?} ({:?})", started.elapsed());
    }
    Ok(())
}
