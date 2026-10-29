# Execution mandates, scope grants and attestation (ADR-035, ADR-036 M5) — closing validation

Date: 2026-10-29. Branch `feat/mandates-layers-attestation`, implementation
`e739a65` (layout fix) and `34e71e0` (measurement, threat model, fixture) on top of the M4 commits (`6efa0d0`, `f18a0f7`, see the
[M4 report](mandates-attestation-m4-validation-2026-10-29.md)). Decisions:
[ADR-035](../docs/adr/035-execution-mandates.md) and
[ADR-036](../docs/adr/036-pulsation-attestation.md). No migration. Nothing is
pushed.

M5 closes Feature 19: the threat model, the measurement the plan asked for,
public documentation, a visual pass in the running application, and the
Windows acceptance. Every check below used hermetic fixtures (scripted model
gateway, temporary workspaces and databases, signing keys in memory). No real
credentials, CLI sessions or paid model calls were used, and the Windows
Credential Manager was not touched. GitHub Actions was not used and is not
evidence here.

## Delivered

- **Threat model.** [cli-threat-model.md](cli-threat-model.md) gains
  "Execution mandates, scope grants and attestation — 2026-10-29": twelve
  threats with their control and the tests that show it, and the residual
  risks. The main one is accepted: a process running as the same Windows user
  can read the key and sign; hardware-bound keys are a later step. A new test,
  `a_record_replayed_on_another_pulsation_is_detected`, copies a signed record
  onto another pulsation: the signature stays valid and the re-derivation
  reports the `pulsation` block.
- **Measurement.** `analyze_pulsation` prints a Governance block (mandate
  revisions by cause, writes the mandate refused, scope requests by state,
  grant rate, files granted, time parked, and the record's state, size and
  seal latency). A database without the tables gets nulls.
  `governance_tolerates_an_older_database_and_flags_refused_writes` covers it
  alone, and `scope_escalation` reads it back after a grant.
- **Fixture.** `review_brief_fixture` gains `--scope-request` (the code unit
  asks for `src/util.rs`, the fixture grants it), `--stage scope` (parked with
  the request pending) and `--stage done` (to the end, waiting for the seal).
- **Visual pass.** It found two defects, both fixed (below).
- **Documentation.** [testing.md](../docs/testing.md) "Governance metrics and
  visual checks"; the status of ADR-035 and ADR-036 and the
  [ADR index](../docs/adr/README.md).

## Visual pass

The release binary ran against fixture data directories (`HVC_DATA_DIR`) and
each window was captured with `PrintWindow` at 1440 × 900 and, for the scope
card, 960 × 640.

| Screen | First capture | After the fix |
|---|---|---|
| Review, failure checkpoint with a pending scope request | The unit label and the path collapsed to zero width, one character per line, and the digest spilled out of the card | Unit, path, digest, reason, risk and budget on their lines at both widths; "Grant and retry" leads the bar |
| Notice of the scope request | Titled "Error" | Titled "Warning", like a waiting review |
| Overview of a finished pulsation: Layers panel and Attestation card | Correct: four layers, "Signed by key … · 5 file(s)", digest, Verify again, Save attestation…, Copy commit trailer | — |
| Overview, Scope layer opened | Each item's label and value collapsed to zero width | Label and value side by side, wrapping inside their half |
| Review, Specification brief, "What approving grants" | Path and agent rows collapsed, overlapping the budget text | Rows readable |
| Review, Integrator brief, Layers card | Headlines collapsed into vertical strips over the next card | Headlines on one or two lines |

The cause is one layout rule. A text that shares a flex row with other
elements had only `min_w_0()`, and GPUI laid it out at zero width. The fix
gives that text `flex_1().min_w_0()` and its neighbours `flex_shrink_0()`,
as the rest of the application already does. The layout tests checked only
horizontal containment, so they passed. They now check the width of each such
text (`assert_readable`), and they fail against the old layout with widths of
0 px, as the regression run showed for the scope card, both Layers views and
the grants card.

Not covered by the pass: Settings → Security, which reads the real credential
store, stays covered by its layout tests only. The plan named a
`pulsation_ui_fixture --layers` mode; it does not exist, and the layers came
from the review fixture's pulsations instead.

## Measurement (fixture `--scope-request --stage done`)

| Metric | Value |
|---|---|
| Mandate revisions | 2 (`issued` 1, `scope_grant` 1), `enforced_v4` |
| Writes and dispatches refused by the mandate | 0 and 0 |
| Scope requests | 1, granted; grant rate 100%; 1 file |
| Time parked at the request | 0.09 s (the fixture decides at once) |
| Record | sealed, signed, outcome `done`, 5 subjects |
| Statement size | 11,931 bytes (the cap is 1 MiB) |
| Seal latency (terminal transition to seal) | 0.146 s |

These are fixture numbers that show the instrument works. The rates and times
of real use are for Feature 17 to collect.

## Acceptance

Environment: Windows 11 Pro 10.0.26300, Rust/Cargo 1.98.0, `--locked`,
`CARGO_BUILD_JOBS=6`, in the branch's own worktree, on the tree of
`34e71e0`.

The Stage 7 gate (`scripts/gate-stage7.ps1`, 2026-10-29 12:23–12:51 local
time, 1,648 s) ran every step. All passed except the advisories step:

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 0.760 s |
| crate dependency boundaries | PASS | 0.206 s |
| cargo fmt | PASS | 5.538 s |
| Clippy (`-D warnings`) | PASS | 30.429 s |
| Rust tests: 56 targets, **1,669 passed, 0 failed, 17 ignored** | PASS | 1,579.688 s |
| GPUI release build | PASS | 21.382 s |
| Windows GPUI smoke (window opened, graceful shutdown, exit 0) | PASS | 2.406 s |
| Rust licenses and sources (560 packages for `x86_64-pc-windows-msvc`, none denied) | PASS | 2.132 s |
| Rust advisories (RustSec `ee1a49a`) | **FAIL: 1 vulnerability**; 14 `unmaintained` and 2 `unsound` informational warnings, the same crates as before | 5.600 s |

Against the M4 gate (`f18a0f7`: 1,667 passed): two more passing tests,
`a_record_replayed_on_another_pulsation_is_detected` and
`governance_tolerates_an_older_database_and_flags_refused_writes`; four
layout tests and one integration test gained assertions. The ignored tests
are the same opt-in ones.

**The advisories failure is not from this branch.** RUSTSEC-2026-0418
("libz-sys build script leaks environment variables in the build
directory", published 2026-10-28, patched in `>=1.1.31`) names `libz-sys`
1.1.29. It entered the RustSec database after the M4 gate's `40f5be6`. The
package has been in `Cargo.lock` since the baseline `175b3cb` and is on
`main` too, through the optional `git2` dependency of `gpui_util`; `cargo
tree` finds it in no build of this workspace, so no binary compiles it.
`cargo audit` reads the whole lockfile, so the step fails. The fix is a
lockfile update of `libz-sys` to 1.1.31, which is a dependency change for
the maintainer to decide; it was not made here.

`scripts/audit-advisories.ps1` run on its own gives the same result.
`scripts/audit-supply-chain.ps1`: technical check passed (560 packages, no
denied licence); distribution stays blocked by the existing GPUI binary
blocker, as before.

`ring`: `cargo tree -i ring` shows `ring` 0.17.14 under `rustls` and
`rustls-webpki`, as at the baseline, plus the direct edge from `hvc-core`.
`Cargo.lock` has 794 packages at `175b3cb` and now; Feature 19 added
dependency edges only (`base64`, `ring` and `zeroize` in `hvc-core`,
`keyring` in `hvc-test-support`), no package.

`scripts/benchmark-ui-latency.ps1 -EnforceThresholds` on the gate's release
binary (idle scenario, 120 samples after 20 warm-up samples): frame-callback
interval p95 6.76 ms and p99 7.11 ms against the 8.33 ms and 16.67 ms
thresholds, **passed**.

Linux headless (`cargo test-headless` in Docker) was not run: this work is
Windows-first, and the Linux pass is a separate step.

## Limits

- Hermetic fixtures only. No real CLI or provider run, and no Linux headless
  run of this branch (Windows-first).
- The record proves integrity since sealing on a machine that held the key,
  not who produced it, nor a trusted time (see the threat model).
- Statement size was measured on one small pulsation; how close large
  pulsations come to the 1 MiB cap is still to be seen in real use.
