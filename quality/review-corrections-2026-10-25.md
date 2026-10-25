# Review corrections — 2026-10-25

First implementation round for the periodic review REV-0001 (2026-10-24). The
review's identifiers (`A0xx` findings, `C0xx` actions) are historical citations
from the maintainers' design archive; this report is self-contained.

Base: `main` at `e8f4644` plus the integration in
[pull request #14](https://github.com/Nathan-dOS/HARD-VC/pull/14) (RustSec
lockfile fix, compatible Dependabot updates, CI timeouts).

## Changes

| Action | Change | Files |
|---|---|---|
| C002 | The editor revision is a SHA-256 over the resolved path and the bytes of one bounded read, replacing `<mtime-ms>:<length>`. A write re-checks the revision once the temporary file is complete, a new file uses `persist_noclobber`, and the response returns the revision of the bytes written. | `crates/hvc-core/src/workspace_service.rs` |
| C004 | After the check command exits, its output readers get 5 s; if a descendant still holds the pipes, the process tree is terminated and the readers get 2 s more before being aborted. A held-open stream, a read error or an abnormal reader task turns an otherwise finished run into an `error` receipt. | `crates/hvc-core/src/orchestration/composition/validation/runner.rs`, fixture mode `exit-leaving-child` |
| C005 | The quota client is built once from one restricted builder; if it cannot be built, requests fail closed with `SourceFailed` and never fall back to a default client. | `crates/hvc-terminal/src/quota/http.rs` |
| C006 | A dictated WAV is owned by a guard created before the first byte, so a partial write, an error, a timeout, a cancel or a dropped future removes it. The voice service sweeps leftover scratch recordings (`<uuid>.wav` only) at start. | `crates/hvc-voice/src/runtime.rs`, `crates/hvc-core/src/voice.rs` |
| C007 | Voice setup cancellation covers the wait for headers, a stalled body (60 s read timeout), `tar` and the runtime checks; nothing is marked or replaced after a cancel. | `crates/hvc-voice/src/runtime.rs` |
| C009 | The distribution policy records that the upstream GPUI licensing issue was closed as completed on 2026-10-07 and the maintainer's 2026-10-25 decision to keep binaries blocked. The blocker stays `open`; the gate still reports `distributionAllowed: false`. | `quality/distribution-policy.json` |
| C011 | `SECURITY.md` describes private reporting through GitHub. `CONTRIBUTING.md` no longer links to a code of conduct that does not exist. | `SECURITY.md`, `CONTRIBUTING.md` |

Not changed in this round: the four CLI pipe paths in
`crates/hvc-terminal/src/terminals.rs` (C004), project-bound workspace
operations in the UI (C003), secret-scanning qualification (C010), the
documentation drift (C012) and the native, performance, live and recovery
campaigns (C014–C017).

## Reproduction on the previous code

The new tests were run against the code before this change where they compile
against its API:

| Test | Previous code | This change |
|---|---|---|
| `revision_detects_an_external_edit_that_keeps_size_and_mtime` | Failed: a stale buffer overwrote the external edit | Passed |
| `revision_is_bound_to_the_file_and_the_project_root` | Failed: identical content gave identical tokens | Passed |
| `cancelling_while_the_server_withholds_headers_ends_the_download` | Failed: the cancel was ignored until the 10 s test guard | Passed in under 1 s |
| `a_hung_command_and_its_children_are_terminated_on_timeout` (Linux) | Hung indefinitely: the grandchild kept stdout open | Ends in about 10 s; fails only its Windows-only process-tree assertion |

## Validation

Environment: Linux x86_64 container, Rust/Cargo 1.98.0, `--locked`, two test
threads, `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0` (debug information
off to fit the disk; test selection and semantics unchanged). Only the headless
crates build there; `hvc-ui` and `hvc-app` are validated by the Windows CI.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check`, `git diff --check` | PASS |
| Clippy, headless crates, all targets | No warning in the changed files. The existing Linux-only warnings in `terminals.rs`, `app_core.rs` and `voice.rs` (`#[cfg(windows)]` code) are unchanged; Windows Clippy with `-D warnings` runs in CI. |
| `workspace_service::` (hvc-core lib) | 43 passed, 0 failed |
| `quota::http` (hvc-terminal lib) | 4 passed, 0 failed |
| `hvc-voice` | 15 passed, 0 failed |
| `voice::` (hvc-core lib) | 3 passed, 0 failed |
| `composition_validation` | 6 passed, 1 failed: the Windows-only process-tree assertion of `a_hung_command_and_its_children_are_terminated_on_timeout` |

Headless regression gate, `cargo test --locked --no-fail-fast` on the six
headless crates:

| Tree | Targets | Passed | Failed | Ignored | Filtered | Duration |
|---|---:|---:|---:|---:|---:|---:|
| Base (pull request #14 head `59dc637`), hanging test skipped | 48 | 1,102 | 27 | 13 | 1 | 1,163 s |
| This change | 48 | 1,117 | 28 | 13 | 0 | 1,148 s |

The 15 additional passes are the new tests. The failure sets differ only by
`a_hung_command_and_its_children_are_terminated_on_timeout`, which the base
run had to skip because it never ends on Linux. The other 27 failures are the
known Linux gaps (CLI adapters, Windows paths and process control) and occur
identically in both trees.

## Limits

- Windows-only behaviour (Job Object termination of the child in
  `exit-leaving-child`, the voice installer) is covered by the Windows CI of
  the pull request, not by the Linux runs above.
- A checksum followed by a rename is not an operating-system compare-and-swap:
  a writer that lands between the final check and the replace can still be
  overwritten.
- `SECURITY.md` assumes private vulnerability reporting is enabled in the
  repository settings.
