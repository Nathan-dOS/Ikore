# Native Whisper large-v3-turbo Q5_0 qualification

Date: 2026-10-29. Status: **completed for local technical execution**. The final
complete workspace suite, production installation/transcription smoke, strict
Clippy, structure/format checks, debug/release builds, native application smokes
and legacy cleanup passed. The Portuguese comparison records an observed
accuracy tradeoff; it does not establish equal or superior recognition quality
or certify a complete 6–8 GB Windows system.

## Decision and scope

The maintainer authorized replacing Python Whisper medium with the native
whisper.cpp CPU backend and Whisper large-v3-turbo Q5_0. Nemotron 3.5 ASR 0.6B
through NeMo-Speech.cpp remains an option with its existing installation.
The replacement fixes Portuguese (`pt`) and transcription, uses one process per
dictation and requires no Python, PyTorch, CUDA, FFmpeg or resident speech
service. The saved backend id is `whisper-large-v3-turbo`; the legacy
`whisper-medium` value is accepted as a migration alias.

The [2026-10-26 medium report](whisper-medium-validation-2026-10-26.md) records
the earlier Python/CUDA implementation and is preserved unchanged. Its results
do not qualify this replacement. Current design is in
[ADR-029](../docs/adr/README.md#adr-029-companion-notes) and the
[architecture](../docs/architecture.md).

## Pinned artifacts

| Artifact | Source pin | Bytes | SHA-256 |
|---|---|---:|---|
| [whisper.cpp Windows x64 CPU archive](https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip) | 1.9.5 / b5454, commit `d1be6fde11ac6e0407606b4e42fe72d34add8037` | 8,928,640 | `6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb` |
| [large-v3-turbo Q5_0 model](https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3-turbo-q5_0.bin) | revision `5359861c739e955e79d9a303bcbc70fb988958b1` | 574,041,195 | `394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2` |

The model occupies 574 MB (547.4 MiB) in the download. This is an artifact size,
not the process peak or the RAM requirement of Windows plus HVC.

## Automated validation

The final `cargo test-all` completed with **exit code zero, 1,913 passed, zero
failed and 17 ignored across 59 test-target/doctest summaries**. Evidence is
`target/whisper-cpp-validation/test-all-final.log` and
`target/whisper-cpp-validation/test-all-final-summary.json`. Ignored opt-in
tests are not counted as executed.

The first complete run had the same counts (`test-all.log` and
`test-all-summary.json`). Strict Clippy then reported three documentation-comment
format issues (missing backticks around identifiers); these and the module
comments describing both backends, language modes and archive layouts were
corrected without changing executable behavior. The final complete suite above
ran after those comment changes. Functional source remained frozen between
the two suites.

The passing coverage includes native CPU argument construction with separate
path arguments, Portuguese output decoding, the true PCM duration rather than
Whisper's 30-second padding, malformed/truncated WAV rejection, valid silence,
missing/malformed/oversized JSON rejection, transcript-file removal on success,
error and dropped futures, cancellation and timeout, and native artifact pins.
Core/UI tests cover legacy-id migration to the canonical id, independent
readiness, persisted selection, switching guards, note capture, the two menu
choices and voice-setting search.

| Check | Result | Evidence |
|---|---|---|
| Final complete workspace suite, including doctests | Passed, exit 0: 1,913 / 0 failed / 17 ignored across 59 summaries | `target/whisper-cpp-validation/test-all-final.log`, `test-all-final-summary.json` |
| GPUI-only structure | Passed | `target/whisper-cpp-validation/structure-check.json` |
| `cargo fmt --all -- --check` | Passed | `target/whisper-cpp-validation/structure-check.json` |
| Crate boundaries | Passed | `target/whisper-cpp-validation/structure-check.json` |
| `git diff --check` | Passed | Executed command result |
| Rust source/license audit | Check passed; existing distribution disposition unchanged | `target/whisper-cpp-validation/supply-chain.log` |
| Advisory audit | Zero vulnerabilities; 14 unmaintained and two unsound informational warnings | `target/whisper-cpp-validation/advisories.log` |
| Final strict workspace Clippy | Passed after all comment corrections: 35.94 s; earlier pass 58.38 s | `target/whisper-cpp-validation/clippy-final.log`; earlier `clippy.log`, `build-checks.json` |
| Debug application build | Passed: 98.43 s | `target/whisper-cpp-validation/build-debug.log`, `build-checks.json` |
| Release application build | Passed: 759.32 s | `target/whisper-cpp-validation/build-release.log`, `build-checks.json` |
| Native application smokes | Isolated debug and release windows observed and closed; release exit 0; normal debug profile Ready; no microphone recording | `target/whisper-cpp-validation/gui-smoke-result.json`, `gui-release-result.json`, `gui-native-ready.json` |

The `gate-stage7` wrapper was **not invoked**. Its checks were executed and
recorded individually; native GUI smoke used Computer Use and isolated data
profiles. The audits do not change the project's existing binary-distribution
decision. Cargo also emitted its existing future-incompatibility notice for
`proc-macro-error2` 2.0.1; the final Clippy command still exited zero.

## Portuguese samples and memory

The production installer was run through `hvc-voice/examples/voice_probe.rs`
against the normal application voice cache. The native runtime and quantized
model reported ready, and the production transcriber returned the exact
reference for the existing 10.944-second Portuguese TTS WAV in **17.477
seconds**, including process startup and model loading. It reported language
`pt` and the source duration 10.944 seconds. Evidence:
`target/whisper-cpp-validation/production-install.log`. This synthetic smoke
verifies execution and basic recognition; it does not establish accuracy for
the user's microphone.

### Same-input human-speech comparison

Three recordings from [Google FLEURS `pt_br` validation](https://huggingface.co/datasets/google/fleurs),
revision `70bb2e84b976b7e960aa89f1c648e09c59f894dd`, dataset ids **1565, 1550 and
1578**, total **33.06 seconds** and **59 reference words**. Every backend
received the same PCM16 mono 16 kHz conversions. Original IEEE float32 WAVs and
the conversion SHA-256 values are retained in the local evidence.

| Backend | Word edits / reference words | Corpus WER | Total elapsed | Highest process peak working set | Highest sampled private commit |
|---|---:|---:|---:|---:|---:|
| Native Whisper large-v3-turbo Q5_0 | 7 / 59 | 11.86% | 52.42 s | 814.0 MiB | 1,305.2 MiB |
| Previous Python Whisper medium | 3 / 59 | 5.08% | 64.81 s | 4,902.5 MiB | 7,384.0 MiB |
| Preserved Nemotron 3.5 ASR 0.6B | 7 / 59 | 11.86% | 10.83 s | 1,792.9 MiB | 5,960.5 MiB |

The native turbo backend used substantially less resident memory than Python
medium, but **made more word errors in this small sample**. This is an observed
quality loss, not quality equivalence. Nemotron was fastest here. Three
recordings do not establish a general accuracy ranking, and the comparison
does not isolate model, quantization and runtime effects.

Conditions: CPU with four threads, a fresh process for each recording, OS disk
cache not flushed, and elapsed time including startup/model loading. Both
Whisper paths fixed Portuguese; Nemotron retained production automatic language
selection. Scoring applies Unicode NFKC and casefold, replaces punctuation with
spaces, collapses whitespace and retains accents/numbers. Corpus WER is total
word edit distance divided by the 59 reference words.

Working set is resident process memory. Private commit is a separate metric,
sampled every 40 ms, and can exceed working set; neither includes Windows or
HVC. Exact byte maxima for turbo were **853,528,576** working set and
**1,368,559,616** sampled private commit. Evidence is
`target/whisper-cpp-validation/results/fleurs/comparison.json` and
`comparison.md`, plus per-process stdout/stderr, native JSON and profiles. The
JSON preserves each reference, transcript, command, timing and memory metric;
`samples/pcm-conversion.json` records input conversion hashes.

The test host has **32 GB RAM**. The intended target is a machine with 6–8 GB
total RAM, which must account for Windows and HVC alongside the transcriber.
These process measurements do not certify that total-system target, all
low-power CPUs or memory peaks for the maximum 120-second capture.

## Installation and cleanup

The production installation is present under the application voice cache. The
extracted runtime occupies **23,147,590 bytes** and the model **574,041,195
bytes**, approximately **597 MB combined**; the runtime install marker and
model hash match the pinned artifacts.

Computer Use observed the rebuilt debug application in an isolated profile:
the native window appeared, its database was created, the model menu switched
recognizers, and Alt+F4 closed the window and ended its process. The temporary
smoke-profile database was later removed. The normal user profile was then
observed in Voice dictation with **Whisper large-v3-turbo Q5_0 selected**, model
**downloaded**, **whisper.cpp 1.9.5 installed**, and state **Ready**. Existing
push-to-talk remained enabled with **Alt+Shift+F1**. Evidence:
`gui-smoke-result.json` and `gui-native-ready.json` under the validation
directory. A separate fresh isolated **release** profile then created its
database, displayed a native window and closed through Alt+F4 with **exit code
zero**, recorded in `gui-release-result.json`; its temporary profile was
removed. The normal debug profile remains open and Ready. **No microphone
audio was recorded in these manual smokes**, so physical microphone and
shortcut-to-recording behavior are not newly qualified by them.

`target/whisper-cpp-validation/cleanup-plan.json` inventories **6,426,375,620
bytes** in the obsolete HVC-owned Python environment, medium checkpoint and its
hash marker. After the legacy CPU comparison, the cleanup was executed:
`cleanup-receipt.json` confirms removal of those **6,426,375,620 logical bytes**,
`LegacyStillExists=false`, `MediumStillExists=false`, and preservation of native
Whisper, Nemotron and the system Python.

`staging-cleanup.json` confirms removal of **608,157,163 additional logical
bytes** in disposable copied artifacts and the isolated smoke profile, while
preserving samples and result/log evidence. After cleanup, production readiness
probes for **both** native Whisper and Nemotron exited zero and reported
`runtime_ready=true` and `model_ready=true` in `native-ready-after-cleanup.log`
and `nemotron-ready-after-cleanup.log`.

No task branch or worktree was created. No commit, push or publication was
performed. Local technical qualification is complete within the limitations
above; the historical medium report remains unchanged.
