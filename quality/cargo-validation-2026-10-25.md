# Rust/Cargo candidate validation — implementation and qualification

> Historical pre-integration record. The current ADR-034-integrated result is
> [2026-10-26 qualification](cargo-validation-2026-10-26.md). Pending items below
> describe the earlier tree and are not the current verification table.


Date: 2026-10-25 (America/Sao_Paulo). Scope: feature 16, first increment M1/M2,
pipeline v4 only. Decision: [ADR-033](../docs/adr/033-cargo-candidate-validation.md).
Operating contract: [Cargo candidate validation](../docs/cargo-validation.md).
Additive SQLite migration: V52.

**Final qualification: PENDING.** This report separates measured intermediate
checks from final-tree reruns and platform gates. A pending/not-executed gate is
not a pass. The latest source-capture race guard was added after the intermediate
test results below; those results alone do not qualify that final change.

## Source and environment

- Implementation baseline: HARD-VC
  `11af87eb2bc3132af2fe090f4c4964c5183706b8`; associated design baseline:
  Cerebro-HVC `159576fc95aa2df0985f091c6ce8de539f760405`.
- `git pull --ff-only` was run in both repositories before implementation and
  brought no delta. Source changes are currently a working-tree implementation;
  final commit/source fingerprint: **PENDING**.
- Validation host: Linux managed environment. The project runner's Windows
  process-control and GPUI desktop gates require separate physical execution.
- Development dependency fetching was explicitly authorized to prepare this
  validation environment. `Cargo.lock` was not changed by that preparation.
  Product Cargo profiles remain locked/offline; development setup does not grant
  the product permission to fetch/install dependencies or toolchains.

## Implemented first increment

- Explicitly approved version 2 Cargo workspace profiles, using locked/offline
  commands, with existing/custom profiles preserved and no automatic enabling.
  Settings accepts a 1–900 second timeout; saving an inspected profile/policy
  leaves it disabled until separately enabled.
- Baseline captured before unit dispatch, complete manifest and durable frozen
  execution context; restart/repair uses the same baseline and policy/tool
  bindings. Candidate and baseline comparison are physical copies, not hardlinks.
- Byte-preserving capture for approved binary assets and safe local inputs,
  subject to 1 MiB per file, 64 MiB total and 10,000 files. Capture checks source
  identity, including the guard against changed source bytes during capture.
  Opaque assets do not become model context.
- Exact host-generated stage-document exclusions recorded as `excludedPaths`;
  legitimate project documentation stays an input. The Test report published
  after checks must not make its own software-validation receipt stale.
- Preparation probes for local Cargo/rustc/rustdoc and locked/offline metadata;
  one shared deadline for preparation and execution, bounded/sanitized evidence,
  explicit unavailable states and no automatic install/download.
- Host-only policy and execution-details sidecars while retaining the V1 wire
  DTOs. Preparation, process state, interpretation, result and candidate integrity
  remain distinct. Unsupported required output, incomplete evidence or changed
  inputs cannot become success because a process exited 0.
- Full freshness/reuse bindings: candidate/input set, command/profile,
  policy/parser, tools, relevant environment and known nonsecret Cargo config
  content. Credential files are not read to build that fingerprint; external
  dependency caches remain a reproducibility limitation.
- Existing ADR-032 Integrator evidence and gaps extended, manifest schema 2,
  sanitized interpretation/result/limitations and legacy-field defaults. UI and
  reports expose execution evidence without granting new authority to the model.

Feature 10 risk M3 and reconciliation M5, spans, other ecosystems, dedicated
security profiles and mutation testing are outside this increment. They are not
declared delivered here.

## Acceptance and test coverage

| Area | Required evidence / test target | Final-tree status |
|---|---|---|
| Profile and policy | Explicit approval, frozen context, no automatic enable/upgrade; unapproved command stops before Code model dispatch | PENDING final rerun |
| Candidate capture | Workspace/path dependencies, binary asset, complete manifest, immutable copies/resume, limits, unsafe paths, source-change race guard | PENDING final rerun |
| Preparation and runner | Real offline check/test, metadata/tool versions, shared deadline, missing local tools/cache, parser/integrity failures | PENDING final rerun |
| Freshness and report exclusion | Final receipt stays fresh after Test report; asset edited outside candidate is stale; config/profile/policy/tool/environment changes invalidate reuse | PENDING final rerun |
| Integrator boundary | New details/defaults, prompt/manifest sanitization and escaping, receipt IDs absent from prompt, unknown evidence remains a gap | PENDING final rerun |
| Persistence | V51→V52, policy/details/context round-trip and cascade, historical rows readable | PENDING final rerun |
| UI | Production compilation and test-target typecheck; physical configuration/review interaction | Intermediate typechecks measured; physical QA NOT EXECUTED |
| Full physical qualification | Windows process tree, timeout/cancel/shutdown, desktop smoke and Stage 7 checks | NOT EXECUTED on this Linux host |

The four new `composition_cargo` integration scenarios are:

- `cargo_inputs_and_policy_are_frozen_before_dispatch_and_an_unavailable_run_keeps_evidence`;
- `a_new_cargo_command_without_an_approved_policy_stops_before_the_code_model_is_called`;
- `the_integrator_keeps_the_final_cargo_receipt_fresh_after_publishing_the_test_report`;
- `a_successful_cargo_receipt_does_not_approve_an_asset_changed_outside_the_frozen_candidate`.

## Measured intermediate checks

These observations precede the last source-capture race fix where stated. They
are retained as intermediate evidence and must not be relabelled final-tree
results without executing the final reruns.

| Check | Observed result | Boundary |
|---|---|---|
| Initial core validation-filter tests | 35 passed | Earlier implementation snapshot |
| `cargo test --locked --offline -p hvc-core --lib composition -- --test-threads=2` | Exit 0; 167 passed, 0 failed, 0 ignored; 20.54 s | Before the final source-capture race guard; `hvc-feature16-core-composition-final.log` |
| `cargo test --locked --offline -p hvc-test-support --test composition_cargo -- --test-threads=2` | Exit 0; 4 passed; 52.48 s | Before the final source-capture race guard; fixture loopback access enabled for the test transport |
| `hvc-core` production check | Passed | Compilation/typecheck, not a complete test or platform gate |
| `cargo check --locked --offline -p hvc-ui` | Exit 0; 7 min 17 s | Compilation/typecheck; GPUI was not rendered; `hvc-feature16-ui-check.log` |
| `cargo check --locked --offline -p hvc-ui --tests` | Exit 0; 4 min 06 s | Test code typechecked; UI tests were not executed; `hvc-feature16-ui-tests-check.log` |

The initial 35-test filter overlaps the later composition run and is not added
to its total. Named logs remain in the validation environment's temporary
directory; they are not represented as committed repository artifacts.

Strict command observed: `cargo clippy --locked --offline -p hvc-core -p hvc-ui
--all-targets --no-deps -- -D warnings`, exit **101**
(`hvc-feature16-clippy.log`). It found pre-existing Linux warnings plus new
implementation lints. New lints were corrected and are being rerun; strict
qualification has not passed.

A diagnostic command uses the same targets with `-D warnings -A unused-mut
-A unused-imports -A clippy::drop-non-drop` to isolate remaining findings. The
first diagnostic run also failed on doc-Markdown lints in the capture example;
its final rerun is **PENDING**. An allowance-based diagnostic run must not be
reported as the original strict command passing.

## Final reruns and gate status

Exact final commands, final source fingerprint and outputs: **PENDING**. Update
this section only from completed command results.

| Gate | Final result |
|---|---|
| Composition/validation unit tests after final race fix | PENDING |
| `composition_checks`, `composition_review` and four new Cargo integration flows | PENDING |
| Migration/persistence regression checks | PENDING |
| Manual capture-only HVC inspection example | PENDING |
| Final formatting and diff integrity | PENDING |
| Strict Clippy core/UI all targets | Observed FAIL, exit 101; final rerun PENDING after new-lint corrections; pre-existing Linux findings remain unwaived |
| Full Windows Stage 7 / physical Job Object checks / desktop smoke | NOT EXECUTED — Windows execution required |
| Physical GPUI review/configuration QA and audio smoke | NOT EXECUTED |

The strict-Clippy investigation identified unchanged Linux findings:
`unused_mut` in `crates/hvc-core/src/app_core.rs:67` and
`clippy::drop_non_drop` in `crates/hvc-core/src/voice.rs:267,343`. Existing Unix
warnings in `hvc-terminal` and unused imports in the UI time module also occur
outside this feature's changes. They are not waived or silently counted as a
strict pass. The final rerun outcome remains to be recorded; `app_core.rs` and
`voice.rs` have no diff in this implementation.

The capture-only inspector, when run, reports file/byte counts and a candidate
hash after capture/restore/verification. It does not compile/test the target
project and does not replace real Cargo validation or Windows gates. No values
from that still-pending run are asserted here.

## Limits and remaining qualification

- Trusted-project execution is not a security sandbox or a hermetic build.
  Offline Cargo does not isolate arbitrary project scripts from the OS/network.
- Known tool/config/input fingerprints do not establish complete knowledge of
  arbitrary external reads or immutable dependency caches.
- Receipt success requires supported interpretation and preserved integrity;
  bounded excerpts are not full logs, unknown totals are not zero, and a
  pre-existing failure remains a failure.
- This work does not claim improved defect-detection quality by the model,
  completed security/mutation support, a full-workspace test run, physical UI
  approval or release/distribution authorization.
- The final reruns, source fingerprint and Windows/desktop gates above remain
  explicit until their actual evidence is recorded.
