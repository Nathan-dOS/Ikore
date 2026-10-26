# Cargo candidate validation — integrated implementation evidence

Date: 2026-10-26, America/Sao_Paulo. Feature 16, first increment M1/M2, pipeline v4.
Design: [ADR-033](../docs/adr/033-cargo-candidate-validation.md).
Operation: [Cargo validation](../docs/cargo-validation.md).

The implementation is complete for the approved Cargo increment. Automated
validation on Linux found **227 distinct passing tests and one known CLI-fixture
portability failure**. Windows process-tree/desktop qualification remains open;
the full platform/release gate is not declared passed.

## Integrated source and delivered behavior

- `git pull --ff-only origin main` incorporated HARD-VC
  `456c91dedeba5e7c96fd1ea15c1305acc77ba4d5`; Cerebro-HVC remained at
  `159576fc95aa2df0985f091c6ce8de539f760405`. Feature 10's ADR-034 risk,
  preflight and reconciliation implementation was preserved.
- Cargo check/test presets require explicit approval, run the workspace with
  `--locked --offline`, and never install tools or fetch dependencies. Settings
  exposes toolchain, asset selectors and the 1–900 second shared budget.
- The host captures baseline bytes before dispatch, checks admitted source
  hashes, restores the same baseline after restart and uses independent copies
  for execution/repair. Assets remain local, outside model context. Inputs are
  verified again after execution.
- Migration **V55** follows ADR-034's V52–V54. It adds policy, receipt details
  and frozen-context tables while preserving historical profile/receipt V1 JSON
  and existing approvals.
- Review manifest **schema 3** retains v1/v2 readability, ADR-034 risk fields
  and Intake-frozen policy identity. Receipt bindings include candidate/inputs,
  command/policy/parser, real tools and relevant environment/configuration.
- Interpreted failed checks are executed negative evidence, never approvals.
  Unknown, partial, stale or invalid evidence cannot satisfy qualified coverage
  or suppress the existing high-risk acknowledgement. A fresh Test receipt
  supersedes only the obsolete Code `CheckStale` gap; the historical row remains.
- Exact host-owned stage documents are excluded from capture. Ordinary source
  documentation and reconciliation edits remain subject to freshness.

Source is a working-tree implementation on the baseline above. The fingerprint
below covers 31 modified/new Rust and migration files, ordered by relative path,
using `path NUL SHA-256(file bytes) NUL`; documentation is excluded:

`6940e6cc9bc0bfd6ea61303ed8143df7259f2c22ba19d9ae57b260ea38da5c16`

## Verification

| Check | Observed result |
|---|---|
| `cargo test --locked --offline -p hvc-core --lib composition -- --test-threads=2` | **187 passed**, 0 failed, 0 ignored; 42.70 s |
| `composition_cargo` integration | **4 passed**; 45.43 s |
| `composition_checks` integration | **5 passed**; 76.10 s |
| `serial_upgrade` integration | **24 passed**, including additive V54→V55 preservation, JSON/FK checks and cascade; 45.26 s |
| `composition_review` | **7 distinct portable cases passed**; **1 known Linux CLI fixture failed** |
| Core production check | Exit 0; 40.57 s |
| UI including test-target typecheck | `cargo check --locked --offline -p hvc-ui --tests`: exit 0; 1 min 11 s |
| Formatting and whitespace | `cargo fmt --all -- --check` and `git diff --check`: passed |
| Strict Clippy | Exit **101**, pre-existing Linux findings described below |
| Clippy diagnostic with the three existing lint classes allowed | Exit 0, core/UI all targets; 1 min 28 s; **not the strict gate** |

The 187 unit and 40 integration passes are distinct; narrower unit filters and
repeated runs are not added again. Provider transports in integration tests use
controlled loopback fixtures. Cargo validation stayed offline.

The integration command was:

```text
cargo test --locked --offline -p hvc-test-support --no-fail-fast --test composition_checks --test composition_review --test composition_cargo --test serial_upgrade -- --test-threads=2
```

That batch exited 101. Review initially passed six cases and failed two: the
known CLI fixture and an assertion still expecting manifest schema 2. Only that
expectation was updated to schema 3. The exact high-risk case then passed
separately (12.23 s), giving seven distinct passing review cases. The CLI failure
was attempted and retained: `a_cli_integrator_reads_host_selected_context_only`
reports `Claude is not ready` because its fixture installs `claude.exe` while
Unix discovery expects `claude`. It is not a request for provider credentials.

The new Cargo flows verify approval before dispatch, frozen assets/policy,
inspectable unavailable preparation, actual offline compilation, fresh Test
replacement of stale Code evidence with a mapped Supported criterion, and an
externally changed asset producing Unverified assessment/coverage.

Strict Clippy command:

```text
cargo clippy --locked --offline -p hvc-core -p hvc-ui --all-targets --no-deps -- -D warnings
```

It fails on unchanged Unix-specific `unused_mut` in `app_core.rs:67` and
`clippy::drop_non_drop` in `voice.rs:267,343`. The diagnostic run appends
`-A unused-mut -A unused-imports -A clippy::drop-non-drop` to check remaining
findings; it passes without treating those allowances as a strict pass. Existing
terminal warnings and the UI time-module imports are outside this change.
Two formatting-only hunks in the newly integrated `tool_scope_ab` test are
included so workspace formatting checks pass.

## HVC capture pilot

```text
cargo run --locked --offline -p hvc-core --example cargo_candidate_capture -- /workspace/HARD-VC crates/hvc-app/assets/icon.ico 'crates/*/fixtures/cli/*/current/hostile.txt'
```

The inspector captured, restored and verified **941 files / 15,291,013 bytes**:
lockfile present, 68 JSONL fixtures, three `src/bin` sources, one ICO and five
opaque inputs. Four opaque inputs are intentional NUL-containing text fixtures.
Both selectors are explicit approvals. No input content is printed or delivered
to a model.

This is a capture-only observation before final evidence-document updates. It
proves resource capture/restoration, not compilation/testing of the whole HVC
workspace. Real check/test execution was exercised on representative Cargo
workspaces. Windows full-workspace build/test, Job Object lifecycle and physical
GPUI configuration/review smoke require a Windows execution environment.

## Evidence and remaining qualification

Logs and machine-readable summaries are retained in the task workspace at
`/workspace/scratch/feature16-2026-10-26/`: `core-composition.log`,
`integration-batch.log`, `review-high-risk-rerun.log`, `cargo-pilot-results.json`,
`ui-check.log`, `clippy-strict.log`, `clippy-diagnostic.log`, `hvc-capture.json`
and `source-fingerprint.json`. The earlier 2026-10-25 report is historical and
cannot qualify the ADR-034-integrated tree.

No full Windows Stage 7, physical process-tree or desktop smoke was executed for
this final tree. ADR-034's earlier Windows results are not inherited by this
feature. Trusted-project execution is not a security sandbox or hermetic build;
Cargo offline mode does not isolate arbitrary project scripts. Dedicated security
and mutation profiles remain later increments of feature 16.
