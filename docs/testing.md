# Testing and validation

Run commands from the workspace root. `rust-toolchain.toml` pins Rust 1.98.0.
The desktop application and GPUI validation target Windows x64.

## Forge assumption reviews

The discovery tests exercise the three-question allowance across replies, an
explicit new round after restart, and the assumption schemas for both provider
formats. Host tests cover a conflicting retention policy, duplicate IDs, stale
revisions, replayed flags, and Unicode answers in the published plan. The native
Forge test saves the current draft as an answer and verifies that reviewing an
assumption opens no model turn.

```powershell
cargo test --locked -p ikore-core -p ikore-ui forge_assumptions
```

## Local commands

The aliases in [.cargo/config.toml](../.cargo/config.toml) use `--locked` and
two test threads per binary:

| Command | Selection |
|---|---|
| `cargo test-contracts` | Shared contracts and serialization compatibility |
| `cargo test-terminal` | Shared contracts and native terminal subsystem |
| `cargo test-core` | Shared contracts, terminal and core host integration |
| `cargo test-ui` | UI |
| `cargo test-editor` | Editor and consuming UI |
| `cargo test-support` | Harnesses, integration tests and consuming UI |
| `cargo test-all` | Complete workspace, including test-enabled examples and doctests |
| `cargo test-unit` | Workspace `lib` and `bin` tests (CI `unit` job) |
| `cargo test-integration` | `tests/` targets and the `pulsation_ui_fixture` example (CI `integration` job) |
| `cargo test-headless` | Crates without GPUI, run by the Docker harness |

`test-unit`, `test-integration` and `cargo test --locked --workspace --doc`
together select the same tests as `test-all`.

Use the relevant area during development. For a complete delivery, follow the
repository's checks:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-all
```

`cargo test` without workspace/package selection uses only `default-members`
(`ikore-app`). Testing a package does not execute its dependencies' tests.
Confirm that a filter selects real tests before using it:

```powershell
cargo test --locked -p ikore-core --lib usage:: -- --list
cargo test --locked -p ikore-core --lib usage:: -- --test-threads=2
```

Aliases already contain `--`; use `cargo test-ui --list` without adding a second
separator. Keep each checkout's own `target/`. Compare timings only with the
same selection, toolchain, cache conditions and concurrency.

## SQLite security and compatibility

The `persistence_service::sqlite_runtime_tests` selection checks the linked
engine against SQLite 3.54.0, the security floor for RUSTSEC-2026-0459. A
separate behavioral test uses 512, 4096 and 65536 byte pages with real WAL
files, multiline Unicode payloads, reader snapshots, committed writes and
rollback. It runs the application's migrations while the WAL is live, reopens
the database and checks content, unchanged migration history, integrity and
foreign keys. It does not claim to reproduce the crafted-file memory leak;
the linked-version check and advisory audit enforce the patched engine.
Unsigned revision/counter coverage retains `fallible_uint` conversions, tests
zero and the largest signed SQLite integer, and rejects negative reads and
overflowing unsigned writes instead of wrapping them.
Migration-fixture coverage verifies that every supported V1-V13 target stops
at exactly the requested version with no later migrations, and rejects zero,
unsupported and overflowing targets before converting to the migrator's signed
version type. Existing application upgrade tests still run in the full suite.
The [Companion record](../quality/companion-dock-validation-2026-11-06.md)
records executed results and the complete regression gate.

## Paragraph layout and fractional DPI

The `selectable_message::tests::paragraph_fusion_` selection compares the
production paragraph renderer with a literal copy of the previous tree.
It checks real Taffy-node reduction, exact geometry/carets/styles, intrinsic
constraints, accumulated height, native wrap thresholds and full clipboard
selection in both directions. The test window uses actual scale factors
1.0/1.25/1.5/1.75/2.0 through its resize callback, rather than substituting rem
size for DPI. Assistant/user layout harnesses cover the relevant geometry;
they do not instantiate the full application or certify physical Windows fonts.
Each drag direction and SelectAll start with a click-collapsed selection and
a fresh clipboard sentinel. Copy with that empty selection must preserve the
sentinel, so neither a stale range nor an earlier clipboard value can conceal
an ineffective gesture. The subsequent copy must produce the complete text.

The last drag point lies in the final paragraph's bottom margin, after the
last caret. The initial draft ended inside a rounded right edge and omitted
the final character even with the legacy renderer; the corrected gesture
keeps the complete-text assertion. The corrected RED passed three cases and
failed only the expected three-versus-two node assertion. The first GREEN
passed all four. Full workspace/native/speed results remain in the
[Companion record](../quality/companion-dock-validation-2026-11-06.md).

GPUI's private `dpi_and_layout_tests` separately check default scale 2.0,
real resize/DPI propagation, rejection before mutation of invalid scales,
and the real frame-node getter. That getter is compiled only for tests or
`test-support`, and returns `None` while the layout engine is temporarily
taken for measurement. Private vendor tests require the temporary offline
harness; dependency tests are not executed by normal UI tests.

## Isolation

Automatic Codex calls confined to host-selected context must retain the local
rollout used to verify model-less public item events. The hardening tests cover
all three modes across official backends; CLI executor fixtures reject
`--ephemeral` for Feature discovery, Notes Scribe and Documentation Reconciler,
and require it for actual Companion rounds. Missing or changed model evidence
continues to fail closed. The opt-in
`authenticated_codex_turn_context_model_smoke` qualifies pipeline and those
three automatic roles against a signed-in Codex CLI using synthetic prompts.
See the [2026-10-31 validation record](../quality/codex-host-context-model-evidence-2026-10-31.md).

Forge Incubation regression coverage checks multiline composer geometry at
narrow and wide widths, explicit file-path requests reaching one ordinary
Feature Agent turn, replay without reopening or redispatch, and duplicate
pending-decision prevention. Discovery context tests keep the full current
feature, brief and developer message while omitting only older whole
conversation turns to fit the configured character budget. An oversized
required context must still fail before a provider call. See the
[2026-10-31 follow-up record](../quality/forge-incubation-path-context-validation-2026-10-31.md)
for execution status and limits.

Forge destination regressions cover automatic file planning for greenfield,
existing and manifest-only projects; documented architecture decisions for
unknown stacks; optional manual overrides; bounded/truncated inventories; and
rejection of traversal, directories, private/generated/policy paths and links.
Batch budget tests count the shared model-facing projection in Unicode
characters, preserve selected criteria and approved scope, exercise exact
limits and v3/v4 capacity, and keep unselected backlog drafts from blocking
legacy handoffs. Integration scenarios cover frozen destination propagation,
unchanged approved revisions, request replay and older manifests. Native UI
scenarios cover automatic destination guidance, optional overrides and
approval/wave refresh without a restart. See the
[2026-11-02 record](../quality/forge-destinations-validation-2026-11-02.md)
for which checks have actually run. Focused selections are:

```powershell
cargo test --locked -p ikore-core --lib feature_forge::batch_budget -- --test-threads=2
cargo test --locked -p ikore-core --lib feature_forge::destinations -- --test-threads=2
cargo test --locked -p ikore-core --lib feature_forge::tests -- --test-threads=2
cargo test --locked -p ikore-test-support --test composition_validation -- --test-threads=2
cargo test --locked -p ikore-ui forge::tests -- --test-threads=2
```

Focused success does not replace the complete workspace regression gate.

Use temporary directories, ephemeral SQLite databases, isolated keyrings and
controlled loopback providers. Opt-in tests requiring authenticated CLIs,
external services or paid quota retain their documented manual prerequisites.
Do not run ignored tests merely to increase the passing count.

For manual application checks, use an isolated profile:

```powershell
$env:HVC_DATA_DIR = "$PWD\target\dev-data"
cargo run -p ikore-app
```

## Complete gate and current evidence

### Companion dock and feature starts

The [2026-11-06 Companion record](../quality/companion-dock-validation-2026-11-06.md)
tracks qualification of the dock, durable drafts, decision projection and
Approve and start. Until that record contains executed results, the scenarios
below describe required coverage, not a passing delivery gate.

- Chat state tests cover simultaneous conversations, inactive-conversation
  replies, isolated cancellation/error state, non-destructive same-project
  refresh and stale project/generation results.
- Flow guidance tests reject stale, reserved, unplanned and dependency-blocked
  tasks, preserve standalone tasks, honor known backend/reconciliation blockers
  and select a bounded dependency-ordered batch. Approved features outside the
  current wave lead to Plan waves rather than a misleading Compose action.
- Core dock tests cover monotonic layout/draft updates, empty versioned draft
  tombstones, size boundaries, project/conversation ownership and deletion.
- Decision tests cover every supported kind, stale identities, project scope,
  lazy resolution links and read-only projection. Native checks cover opening
  once without taking focus, badge-only restoration and unchanged guarded
  domain actions.
- Forge tests cover read-only composite preview, wave/dependency/model blockers,
  approval preserved after a later failure, restart/retry recovery, changed
  request input, stale revisions and concurrent replay producing one pulsation.
  A deterministic race advances the feature after task selection is frozen:
  an older start command must neither plan the new revision nor report it as
  planned, including when the new revision is already in Current.
  Question tests cover old payloads, bounded option/recommendation references,
  durable metadata with the same invocation count, and selection that only
  fills the composer for the current discovery turn.
  Transaction regressions verify that preflight sees uncommitted route/provider
  changes on its supplied connection, and that repeated preview reads complete
  while a worker writes, without a nested-reader/committing-writer lock cycle.
- Native layout/smoke scenarios use 960x640 and 1440x900 windows, narrow windows
  and display scaling. Check multiple streams, close/minimize/expand, draft
  restoration and approval shortcuts while an input has focus.
  The large-history launcher test sends a complete Space keydown/keyup gesture:
  keydown alone must not activate, and keyup must close the focused launcher
  exactly once. Pulsation badges deduplicate checkpoints by pulsation while
  keeping the artifact-review count separate from failures and clarifications.
  Composer regressions retain multiline geometry when opening full Chat from
  navigation or Expand, mount the shared input in one surface, and list only
  minimized or overflow topics under Reopen. Alias regressions open history
  over a linked decision, bind a decision after its chat was opened and restore
  legacy layouts in either alias order. They retain one canonical decision
  window, the same composer and unsent drafts, without merging distinct cards.
  Focus regressions send full shortcut down/up gestures before and after hiding
  the dock, closing/minimizing a focused topic, binding Ask Companion or replacing
  Send with Cancel, including changes rendered while the window is inactive.
  A persistent dock ancestor recovers only the same removed child's focus,
  without activating the window; full Chat focus, explicit blur followed by
  removal, and automatic decision opening remain unaffected.
  Unicode input fixtures use Rust escapes for language-specific characters so
  the English-source guard remains enforced without losing Unicode coverage.
  Selectable-message regressions retain unchanged text-piece measurements
  across streamed appends, invalidate inherited text style and rem changes,
  remeasure wrapped text when width becomes unconstrained, and preserve full
  Unicode/table/diff selection and copy through redraws and resizing. Selected
  pieces rebuild their consumed highlight recipe on every layout request.
  The borrowed Markdown renderer passes original paragraph/table storage to
  its callback, preserves complete code/list formatting, and owns text only
  when a cached piece changes. Message buffers reserve capacity without
  omitting any blocks or clipboard content. The separate owned renderer still
  moves its original string storage; parity tests compare block geometry for
  headings, paragraphs, tables, labelled/unlabelled code, quotes, lists and rules.
  Measurement-cache tests use test-support-only miss/retention counters to
  verify repeated exact constraints reuse complete measurements, the four-state
  limit and LRU eviction. Wrapping and truncation widths are independent keys;
  restored states retain Unicode text, hit testing and ellipsis geometry, while
  style/rem changes discard every retained state.
  HVC-GPUI-008 also has six private vendor paint tests in
  `text_system::line::tests`. They inspect Scene operations and the private
  font-metric cache to prove no-color backgrounds skip the font backend while
  preserving empty layers, colored/transparent backgrounds, wrapped Unicode,
  foreground decorations and overlapping order/replay. They are separate from
  `cargo test-all`, since the vendored dependency is not a workspace member.
  The recorded test-only harness temporarily adds GPUI membership, omits three
  unused development-dependency blocks and resolves only existing lock edges;
  all package names/versions/sources/checksums remain identical. The command is
  `cargo test --offline --locked -p gpui --lib --no-default-features --features
  test-support,windows-manifest text_system::line::tests -- --test-threads=2`.
  Backups, the precise harness diff and RED/GREEN logs are in
  `target/companion-final/no-background-2026-11-09/`. Restore manifests and the
  lock byte for byte before normal delivery checks; these test-support features
  do not qualify the production release build.
  HVC-GPUI-009 extends that private selection to 12 cases. A private `cfg(test)`
  visit counter proves off-envelope lines avoid the old glyph loop; tests also
  retain overhang, subpixel/strict boundary contact, a partially visible wrapped
  emoji and taller font, every `Some` decoration, invalid-input fallback and
  exact Scene order/replay. Enumeration checks the envelope against the old
  row predicate using repeated floating-point additions. After the focused
  GREEN, all 82 vendor library tests were listed and passed with the same
  offline/locked features, without a test-name filter. Their logs are in
  `target/companion-final/vertical-culling-2026-11-09/`. The 12 cases are included
  in those 82; neither count is part of the default workspace-suite count.
  Shutdown coverage delays an
  admitted Forge answer until closing, then verifies the final empty draft
  tombstones, newer unsent edits and refused submissions. Chat shutdown retains
  Send/Create admissions while dropping read/cancel/delete tasks. These flushes
  remain subject to GPUI's existing 100 ms shutdown deadline.

List before filtering, then run the whole workspace regression suite. The new
core Forge scenarios can be selected with `approve_and_start`,
`structured_feature_questions` and `discovery_question_details`; the GPUI
scenario is `selecting_a_question_option_only_fills_the_composer_for_the_current_turn`.
Only a complete Stage 7 gate and the separately recorded speed comparison
qualify delivery. Simulated provider responses do not establish real-model
quality or end-to-end provider latency.

The speed-report contract has a separate PowerShell 5-compatible regression
suite: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/tests/gate-speed-tests.ps1`.
It rejects missing/null/non-finite samples, unequal pairing, incorrect probe/UI
schemas, incomplete streaming workload, wrong viewport dimensions, empty or
duplicate fixture observations, non-boolean outcomes and invocation/intervention
regressions. The full gate measures one frozen composition canary across four
arms in every paired sample, then checks full held-out correctness/call parity.
Artifacts are hashed before measurement and checked again afterwards.

```powershell
powershell -ExecutionPolicy Bypass -File scripts/gate-stage7.ps1
```

The gate includes structure, formatting, strict Clippy, workspace tests,
release build, Windows smoke and dependency audits. Skipping an audit does not
certify a complete passing gate.

The [2026-10-23 boundary validation](../quality/rust-boundaries-validation-2026-10-23.md)
records 1,421 passing tests, zero failures and 13 ignored tests across all 45
default workspace targets. Execution used three disjoint selections: the
workspace excluding core, core library tests and core doctests. The isolated
collectors-disabled selection additionally passed ten contract tests and 181
terminal tests, with one ignored test.

Strict workspace/all-target Clippy, formatting, crate boundaries and the
public app build without collectors passed. Nine RustSec findings remain
identical to the original lockfile under the same advisory database; binary
distribution policy remains blocking. See the dated report for release/smoke,
build measurements and the unverified local Docker execution. The
[2026-10-17 integration report](../quality/main-merge-validation-2026-10-17.md)
retains the earlier baseline and its then-current advisory count.

## Linux and Docker

The [Docker Compose services](../docker-compose.yml) run headless
contracts/terminal/core/editor/test-support tools. They are one-shot and behind profiles, so
`docker compose up` starts nothing:

```powershell
docker compose --profile tools build
docker compose --profile tools run --rm test-runner
docker compose --profile tools run --rm cargo-check
docker compose --profile tools run --rm clippy-check
docker compose --profile tools run --rm fmt-check
```

`test-runner` runs `cargo test-headless`. Linux validation has known gaps and
does not certify the Windows desktop. See the [quality index](../quality/README.md)
for evidence and the meaning of historical design references.

## Continuous integration

[`ci.yml`](../.github/workflows/ci.yml) runs `fmt` on every pull request and push
to `main`. Its other jobs run only on manual dispatch: `clippy`, `unit` and
`integration` on `windows-latest`, plus the Docker job `linux headless (docker)`,
which stays informational until the Linux failures are fixed.
[`nightly.yml`](../.github/workflows/nightly.yml) runs the complete Stage 7 gate,
also on manual dispatch only. [`security.yml`](../.github/workflows/security.yml)
runs `cargo audit` on dependency changes and weekly, and the license and source
audit on dependency changes. The jobs call the same aliases as local runs, with
no retries.

The heavy jobs are manual because of cost: a full CI run takes two to four hours
of hosted Windows minutes and a gate about two, while the free plan's monthly
allowance covers about six full CI runs across all of the account's private
repositories. Run `cargo test-all` and `scripts/gate-stage7.ps1` locally instead.
Standard hosted runners are free for public repositories, so restore the automatic
triggers when this repository is opened.

## Secret sanitization qualification

[Secret sanitization](secret-sanitization.md) describes the source/projection and
publication contracts. Relevant tests are in `secret_redactor`, `sanitization`,
CLI/Companion transport fixtures, composition and the review UI. The measured
scanner probe is hermetic:

```powershell
cargo run --locked -p ikore-core --example secret_sanitization_benchmark
```

The probe reports its build profile, sizes and timings. Whole-process peak memory
is measured separately; scanner timing is not a claim about provider latency or
the complete desktop application's memory.

## Integrator review boundary

[Review policy](review-policy.md) describes the reviewer's classified inputs,
evidence gaps, `REVIEW.md` and revision rounds. Pure rules are tested next to
`review_boundary`, `review_policy`, `line_diff`, `settings` and the recipes;
publication protection and base capture in `serial_tests`; the full pipeline in
`ikore-test-support/tests/composition_review.rs`, which also runs the reviewer
through an official CLI fixture and a Custom adapter; V51 in `serial_upgrade`;
the Review tab in the `hitl` and layout tests. Every run is hermetic:

```powershell
cargo test --locked -p ikore-test-support --test composition_review -- --test-threads=2
```

## Tool policy, risk, pre-flight and documentation drift

ADR-034 tests sit next to their rules: the scope table in `ikore-contracts`
`contracts`, the declared policies in `composition::policy`, the CLI refusal
in `ikore-terminal` `terminals::tests`; risk and coverage in
`composition::risk`, `review_boundary`, `review_policy` and
`ikore-test-support/tests/composition_review.rs`; the Forge pre-flight in
`feature_forge::preflight` and the Forge tests (`persistence_service::forge_batches`);
documentation reconciliation in `doc_reconciliation` (links, contract and an
integration suite with a scripted gateway); V52, V53 and V54 in
`serial_upgrade`; the screens in the `forge`, `doc_drift`, `hitl` and layout
tests:

```powershell
cargo test --locked -p ikore-core --lib doc_reconciliation feature_forge::preflight forge_batches -- --test-threads=2
cargo test --locked -p ikore-test-support --test serial_upgrade -- --test-threads=2
```

`documentation_reconciler_runs_companion_scoped_under_the_reconciliation_origin`
in `cli_executor`, like its Notes Scribe twin, needs the Windows `.exe` CLI
fixtures and fails on Linux before the run starts.

## Execution mandates

ADR-035 M1 keeps its pure rules in `orchestration::mandate` (derivation,
canonical digests, the write, transport, executor and check checks, and a
generated-envelope property test) and its host transactions in
`persistence_service::mandate_persistence` (acceptance and publication refusals,
receipts, the preview, the transport amendment, the frozen budget). The full v4
pipeline runs in `ikore-test-support/tests/execution_mandate.rs`; V59 is in
`serial_upgrade`; the grants card in the layout tests. The receipts of the CLI
fallback, clarification and path-confirmation decisions are asserted in those
flows' own tests, and the project-deletion test covers the V59 rows:

```powershell
cargo test --locked -p ikore-core --lib mandate -- --test-threads=2
cargo test --locked -p ikore-test-support --test execution_mandate -- --test-threads=2
```

## Trust layers

ADR-035 M2 keeps the state rules in `orchestration::layers` (one table test per
trigger and threshold edge, legacy and new pulsations, order independence, the
50-item cut) and the reader in `persistence_service::layers_persistence` (a
composed pulsation's mandate, writes and refusals, a tampered mandate, the
detail read under a concurrent writer). `ikore-test-support/tests/trust_layers.rs`
follows a v4 pulsation from the Specification to the end and provokes a model
other than the one requested, a spent repair budget and a failed required
check. The panel, the brief card and the header dot are in the layout tests,
and the report's golden file carries the Layers section. The cost probe builds
a pulsation at the batch limits and times reading and deriving the layers:

```powershell
cargo test --locked -p ikore-core --lib layers -- --test-threads=2
cargo test --locked -p ikore-test-support --test trust_layers -- --test-threads=2
cargo run --locked --release -p ikore-core --example layers_benchmark
```

## Scope requests

ADR-035 M4 keeps the derivation in `orchestration::mandate::scope` (one table
test per refusal rule, a mixed request, the path, request and budget limits,
the risk with the granted files, a Test unit's roles, the amendment of the
mandate) and the host in `persistence_service::scope_persistence`.
`ikore-test-support/tests/scope_escalation.rs` runs a v4 pulsation whose Code
unit asks for a file it only reads: the grant, the new mandate revision and
receipt, the shared budget and the publication; a file changed after the
request; another decision declining it; a Test unit asking for a production
file; a restart between the grant and the dispatch; and a revision that
supersedes the grant. V61 is in `serial_upgrade`; the card, the stale message
and the button are in the layout tests:

```powershell
cargo test --locked -p ikore-core --lib scope -- --test-threads=2
cargo test --locked -p ikore-test-support --test scope_escalation -- --test-threads=2
```

## Attestations

ADR-036 M3 keeps the pure parts in `attestation` (the DSSE pre-authentication
encoding, a golden vector signed with a fixed test seed, envelope tampering,
the key's stored form) and the host in
`persistence_service::attestation_persistence` (sealing once and after the
publications settle, outcomes, the sealing switch, an unavailable, missing or
replaced key, rotation, the immutability trigger, the size cap, a failed
derivation and its retry, a restart before the seal, tampered rows, envelope
and files, export and import, and a scan of every table for the private key).
`ikore-test-support/tests/attestation.rs` seals a v4 pulsation that finishes and
one aborted after writing, and checks the report's Provenance section. V60 is
in `serial_upgrade`; the card, the list mark and the Security page are in the
layout tests; the report's golden file carries the Provenance section.

Every one of these tests keeps the key in memory. Unit tests of `ikore-core` that
build a service on the default store never reach the operating system's
credential store: under `cfg(test)` it reads as empty and refuses writes. The
one test that uses the Windows Credential Manager is ignored by default and
writes only under the test service `ikore.attestation-test`, removing
the entry afterwards. Run it only when you want to check the real store:

```powershell
cargo test --locked -p ikore-core --lib attestation -- --test-threads=2
cargo test --locked -p ikore-test-support --test attestation -- --test-threads=2
cargo test --locked -p ikore-test-support --test attestation -- --ignored --test-threads=1
```

To check a real bundle with OpenSSL as [attestation.md](attestation.md)
describes, set `HVC_ATTESTATION_OUT` to a folder before the integration test;
it saves the finished pulsation's bundle and public key there.

## Governance metrics and visual checks

`analyze_pulsation` (and `scripts/analyze-pulsation.ps1`) prints a Governance
block: mandate revisions by cause, writes and dispatches the mandate refused
(a healthy run has none, and the text says so when there are), scope requests
by state with the grant rate, the files granted and the time parked, and the
attestation's state, statement size and seal latency. A database older than
V59 gets nulls. `governance_tolerates_an_older_database_and_flags_refused_writes`
covers the block on its own, and `scope_escalation` reads it back after a
grant.

`review_brief_fixture` prepares the screens for a manual pass in the real
app: `--scope-request` makes the code unit ask for `src/util.rs` and grants
it, `--stage scope` parks with the request pending, and `--stage done` waits
for the signed record. Start `ikore` with the printed `HVC_DATA_DIR`.
The fixture keeps its signing key in memory, so the app shows the sealed
record without reading the credential store; avoid Settings → Security there,
which reads the real store.

A text that shares a flex row with other elements needs `flex_1().min_w_0()`,
and its neighbours `flex_shrink_0()`. With `min_w_0()` alone, GPUI can lay the
text out at zero width, one character per line. The layout tests of the
Feature 19 cards check each such text with `assert_readable`.

## Editor revisions, check output and voice setup

The workspace revision is a SHA-256 over the resolved path and the bytes read,
so a same-size, same-mtime external edit is a conflict. `workspace_service`
tests cover that edit, another file or root with identical content, a change
while the temporary file is written, a concurrent creation and a deleted file.
The runner fixture `exit-leaving-child` exits while a child keeps its output
open; the receipt must end within the drain bounds and never pass. Quota HTTP
and voice setup tests use loopback servers that never answer or stall the body,
plus a runtime check cancelled mid-run:

```powershell
cargo test --locked -p ikore-core --lib workspace_service::tests -- --test-threads=2
cargo test --locked -p ikore-test-support --test composition_validation -- --test-threads=2
cargo test --locked -p ikore-terminal --lib quota::http -- --test-threads=2
cargo test-voice
```

On Linux the process tree is not contained, so the timeout test that checks
the grandchild stopped fails there by design; it no longer hangs the run.

## Terminal extraction and feature boundaries

Run `scripts/check-crate-boundaries.ps1` to inspect all Cargo edges. Pure
terminal tests do not depend on core or test-support. Integration tests retain
the real host policies and migrations.

The integration harness enables credential collectors. Check the disabled
production graph independently of development targets:

```powershell
cargo check --locked -p ikore-app --no-default-features --bins
cargo tree -p ikore-app --no-default-features -e normal,build,features
cargo test --locked -p ikore-terminal --lib --no-default-features -- --test-threads=2
```

Check the terminal-to-zeroize optional edge, not global absence of zeroize:
other libraries can use zeroize independently. The full workspace tests exercise
the default collector configuration. See [ADR-031](adr/031-terminal-boundary.md).

## Canvas AI authoring candidate

The [authoring implementation](canvas-ai-authoring.md) is published on main in
[`92f03c4`](https://github.com/Nathan-dOS/Ikore/commit/92f03c43bd73521c9f4ee55c51c697ba84f557a8).
Its selected source tree excluded unrelated local voice changes. On 2026-10-29,
the complete suite passed 1,778 tests, zero failed, 16 ignored and zero filtered
across 55 target summaries in 2,023.458 seconds. All nine gate checks passed in
2,933.376 seconds, including the 806.453-second release build and 4.951-second
graceful Windows smoke with exit 0 and its temporary profile removed. All 884
source hashes and Git blobs were verified with zero drift. No task branch or
worktree was created. See the
[publication qualification](../quality/canvas-ai-publication-qualification-2026-10-29.json)
and [publication record](../quality/canvas-ai-publication-2026-10-29.md).
The earlier 1,782-test/887-source result belongs to the historical combined local
checkout; it is separate from this qualification of the published selection.
Focused selections are:

```powershell
cargo test --locked -p ikore-core --lib companion_drawings:: -- --test-threads=2
cargo test --locked -p ikore-core --lib chat_service::canvas_authoring::tests -- --test-threads=2
cargo test --locked -p ikore-test-support --test canvas_semantics -- --test-threads=2
cargo test --locked -p ikore-test-support --test cli_executor canvas_cli_offer_requires_consent -- --test-threads=2
cargo test --locked -p ikore-terminal --lib companion_extracts_canvas_offer -- --test-threads=2
cargo test --locked -p ikore-ui canvas -- --test-threads=2
```

Core/controller tests cover closed schemas, legacy scenes, profile topology,
atomic batches, limits, authorization scope, expiry and idempotency. Durable-store
tests cover source hashes and unavailable sources, target admission/replay,
hidden staging, transactional publication/rollback, cancellation races, restart,
deleted-result tombstones and project cascade. Loopback API tests exercise scoped
tools, configuration drift and in-flight cancellation. The Windows CLI fixture
drives offers, consent and publication across supported backends without an
authenticated provider; it is not a live-model quality test.

The semantic service integration target checks sanitization, save/reopen,
duplication/reference remapping and export. Native UI tests cover explicit
approval/decline/stop, API and CLI target snapshots, stale/scoped callbacks,
restart/event hydration without navigation, explicit result opening, semantic
controls and geometry/semantic Undo/Redo. These selections overlap with the
complete suite and their counts must not be added together. The native semantic
control fixture also checks that warnings appear for an incomplete scene, clear
after a valid labeled addition and return on Undo. The completed gate included
`cargo test-all` and the checks documented above. Authenticated providers and
physical display/DPI/IME behavior remain unqualified; the optional document
resolver is not exposed. Actual results and execution limits are recorded in the
[publication record](../quality/canvas-ai-publication-2026-10-29.md).

Quota meter tests distinguish historical captured windows from live-window
fixtures. Tests needing live windows copy the fixture and set relative future
resets; captured files and production expiry rules remain unchanged.
`claude_run_events_keep_expired_windows_out_of_the_live_meter` separately checks
that expired windows stay out of the live meter. The historical workflow report retains the
time-dependent third gate failure and the 17-test focused correction result.

The `greenfield_principles` selection in `ikore-core` exercises Forge triage with a
workspace created by the real greenfield bootstrap and a scripted Feature Agent.
It checks the delivered prompt and the stored finding. Boundary fixtures cover
root precedence, case and separators, UTF-8 byte budgets, the aggregate budget,
changes to the effective digest, a refused root entry and a Windows junction.
Run `cargo test --locked -p ikore-core --lib greenfield_principles -- --test-threads=2`.
