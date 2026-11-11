# Agent exchange baseline (ADR-037, before M1)

Date: 2026-10-29. Source: `main` at `d1b70e2`, measured on the branch
`feat/agent-exchange` in a fresh worktree with an empty `target/`. The gate ran
on the unchanged tree; the measurement harness was added afterwards and changes
no product code.

This is the comparison point for ADR-037 M1 to M3. It approves no distribution.

## Environment

Windows 11 Pro 10.0.26300, `rustc 1.98.0`, `cargo 1.98.0`,
`CARGO_BUILD_JOBS=6`. GitHub Actions was not used.

## Stage 7 gate

`scripts/gate-stage7.ps1` passed in 2,640 s, building everything from scratch.

| Step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | Passed | 0.9 s |
| Crate dependency boundaries | Passed | 0.2 s |
| `cargo fmt` | Passed | 9.7 s |
| Clippy `-D warnings` | Passed | 149.5 s |
| Rust tests (`cargo test-all`) | 59 targets, 1,913 passed, 0 failed, 17 ignored | 1,735.2 s |
| GPUI release build | Passed | 735.7 s |
| Windows GPUI smoke | Window opened, graceful shutdown, exit 0 | 1.8 s |
| Rust licenses and sources | Passed; 560 packages resolved for `x86_64-pc-windows-msvc` | 2.0 s |
| Rust advisories | `cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `ee1a49a` | 5.0 s |

Binary distribution stays blocked by `zed-55470`, as recorded in
`quality/distribution-policy.json`. The 17 ignored tests are opt-in by design:
authenticated CLIs, network, microphone, global hotkey, manual inspection, or
the real Windows Credential Manager.

## Measurements

The harness was added after the gate on the same branch: three ignored tests in
`crates/hvc-core/src/exchange/baseline_tests.rs` and a per-call capture in the
composition benchmark (`PromptCall`). Timings come from the test (debug)
profile, a temporary file-backed SQLite database with the real schema (V61)
and a rollback journal, on this machine with no other build running.

### Prompt size (fixture benchmark)

`composition_benchmark --mode fixture --held-out`: 20 fixtures in 4 arms, 80
observations, 80/80 correct, 540 provider calls. Calls, rejections and
interventions match the [2026-09-13 report](stage-composition-benchmark-2026-09-13.md)
on the same fixtures.

| Pipeline | Calls | Median chars | p95 | Max | Labelled items (median / max) |
|---|---:|---:|---:|---:|---:|
| v4 (arms B, C, D) | 415 | 6,339 | 8,224 | 8,615 | 6 / 13 |
| Legacy (arm A) | 125 | 11,398 | 14,119 | 14,591 | 2 / 2 |

Simulated v4 input tokens grew since 2026-09-13 (arm B development 121,316 to
151,526, +24.9%; arm C 126,162 to 176,814, +40.1%), so this report, not the
earlier one, is the M1 reference. Per-call data is in
[stage-composition-benchmark-2026-10-29.json](stage-composition-benchmark-2026-10-29.json),
field `prompt_calls`.

With the accepted line format and legend, M1 is estimated to add about 935
characters to a median v4 prompt (1,635 at most) and about 460 to a legacy
prompt. The M1 ceiling is therefore absolute: at most +1,000 characters at the
median and +1,700 at the maximum per v4 prompt, +600 per legacy prompt, with
the same orchestration results.

### Pulsation transitions

Each sample is one `IMMEDIATE` transaction with the transition and its commit.

| Transition | n | p50 | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| Non-terminal (Running to Checkpoint and back) | 1,000 | 7.35 ms | 11.41 ms | 22.89 ms | 31.28 ms |
| Terminal (to Done) | 200 | 3.11 ms | 3.97 ms | 4.22 ms | 5.41 ms |

### Terminal events

| Measure | n | p50 | p95 | p99 | Max |
|---|---:|---:|---:|---:|---:|
| `append_event` | 100,000 | 12.49 ms | 15.26 ms | 26.74 ms | 122.1 ms |
| `list_events`, page of 500 after event 0 | 5 | 1.580 s | 1.582 s | | 1.582 s |
| `list_events`, page of 500 after event 50,000 | 5 | 1.571 s | 1.597 s | | 1.597 s |
| `list_events`, page of 500 after event 99,500 | 5 | 1.579 s | 1.722 s | | 1.722 s |

Appending 100,000 events took 1,203 s. A page costs the same at any cursor
because the whole run is read and redacted before filtering.

### Snapshot manifest

`prepare_snapshot` on this repository: 1,002 files, 17,500,527 original bytes
and 17,395,543 projected bytes. 869 text files are at most 32 KiB
(7,110,919 bytes); the 496 smallest fit in 1 MiB. The current paths-only
manifest is 49,234 bytes; a per-file manifest is estimated at 246,617 bytes.
Preparation took 34.3 s in the debug profile.

## Harness validation

On the branch with the harness: `cargo fmt --all -- --check` passed; Clippy
`-D warnings` on `hvc-core` and `hvc-test-support` with all targets passed;
`hvc-core` library tests: 1,062 passed, 0 failed, 4 ignored (the three baseline
tests and one earlier opt-in test); `hvc-test-support` `composition_benchmark`
tests: 2 passed.

## Rerunning

```powershell
cargo run --locked -p hvc-test-support --example composition_benchmark -- --mode fixture --held-out --output-dir <dir>
$env:HVC_EXCHANGE_BASELINE_WORKSPACE = '<workspace to snapshot>'
cargo test --locked -p hvc-core --lib exchange_baseline -- --ignored --nocapture --test-threads=1
```

Each harness test prints one `AGENT_EXCHANGE_BASELINE` JSON line and asserts
no threshold. Timings depend on the machine and on background load; compare
runs from the same machine only.
