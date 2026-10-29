# Whisper medium alongside Nemotron

Settings → Voice dictation now offers the existing Nemotron backend and official
Python Whisper medium with `language="pt"` and `task="transcribe"`. The choice is
stored in `runtime_settings` as `voice.backend`; absent or unknown values retain
Nemotron. Switching preserves both installations, push-to-talk preferences and
the configured shortcut. Selection is unavailable during setup and dictation.

Whisper has a separate embedded Python 3.12.10 environment, pinned Whisper
20250625 and PyTorch 2.9.1 with CUDA 12.8. Its model is 1,528,008,539 bytes with
SHA-256 `345ae4da62f9b3d59415adc60127b97c714f32e89e936602e85993674d08dcb1`.
Python and the pip wheel are also checked against fixed sizes and SHA-256 values.
Python package dependencies are resolved from PyPI and the official PyTorch
index; their complete dependency graph is not checksum locked. The system Python
is unchanged. The microphone's PCM WAV is decoded directly into a NumPy array,
so this path needs no FFmpeg. Transcription has a ten-minute timeout and supports
cancellation; the shared scratch-audio guard removes the recording afterward.

## Local installation and transcription smoke

The production installer was exercised through `hvc-voice/examples/voice_probe.rs`
against the normal application voice cache. Both runtimes and models reported
ready. Whisper reported CUDA available, an RTX 5060 Ti with 16 GB, and `sm_120`
among the installed PyTorch architectures.

A local Windows Portuguese voice synthesized a 10.944-second mono, 16-bit,
16 kHz WAV. The exact same file was passed through each production transcriber.

Reference:

> Este é um teste de transcrição em português. Quero comparar os dois modelos e escolher qual funciona melhor no aplicativo.

Whisper returned the reference text exactly, with language `pt`, in 28.50 seconds.
Nemotron returned “Este é um teste de transcrição importus. Quero comparar os
dois modelos e escolher qual funciona melhor no aplicativo.”, with language
`pt-PT`, in 4.13 seconds. These timings include process startup and model loading.
This synthetic smoke verifies execution and basic Portuguese recognition; it
does not establish accuracy or latency for the user's microphone recordings.
The GPU path was exercised; the CPU fallback was not benchmarked.

The existing development app was closed normally, rebuilt with
`cargo build --locked -p hvc-app`, and reopened at the same executable path.
Native UI inspection confirmed both choices, Nemotron ready, and then Whisper
selected with its model downloaded, runtime installed and status Ready. The
existing enabled push-to-talk preference and `Alt+Shift+F1` shortcut were retained.

## Automated coverage

The focused core/UI voice selection passed eight tests: default settings,
note capture, short/silent/missing-project handling, persistent model selection,
independent readiness, invalid persisted values, switching guards, both menu
options and voice-page layout. Formatting, crate boundaries and strict workspace
Clippy passed. The full workspace gate stopped at the unrelated
`composition_validation::oversized_output_is_capped_and_marked_truncated_without_deadlock`
integration test: expected `Error`, observed `Passed`. Repeating the same test
alone reproduced the failure. No transcriber is involved in that test, and its
runner and fixture were not changed for this feature. The overall gate is not
certified as passing; its outcome is recorded in
`target/stage7/whisper-medium-timings.json` and `target/whisper-validation.log`.

The voice and UI targets that the abort had prevented from running were checked separately
using the exact binaries compiled by the workspace gate, with two test threads:
`hvc-voice` (15 passed, one opt-in ignored), `hvc-ui` (337 passed), and
`voice_runtime` (two passed, one live-download test ignored). This required an
unsandboxed rerun because the sandbox blocks the loopback HTTP and subprocess
fixtures. The production installation and real-model transcription smoke above
were also performed unsandboxed. Evidence: `target/whisper-remaining-tests-verified.log`.

Rust source/license checks and advisory checks passed technically; the audit's
existing distribution disposition is unchanged. These do not audit the Python
dependency graph.

Both development and optimized release builds passed. The release build took
9 minutes 34 seconds. A separate fresh `HVC_DATA_DIR` profile initialized its
database, displayed the native main window and closed normally through the UI
with exit code zero. The user's normal development profile remained open with
Whisper selected. This manual smoke used Computer Use for window inspection
and closure; it did not download a second copy of either model.

An existing Windows-only `set_readonly(false)` call in candidate capture needed
a narrowly scoped Clippy allowance for the Unix-permission lint. Its behavior
is unchanged; Unix continues to use mode `0644` explicitly.
