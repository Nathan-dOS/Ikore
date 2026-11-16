# Cargo candidate validation

Composed validation failures now support [guided recovery evidence](guided-test-recovery.md)
through an immutable V66 sidecar. A human Code revision receives bounded check
diagnostics and available candidate test evidence. This preserves the runner's
existing command authority, applicability, receipt semantics and failure gates;
it does not automatically revise a test expectation or an approved requirement.

Status: the first Rust/Cargo increment is implemented and integrated with
ADR-034. Automated verification and the remaining Windows qualification are
recorded in the [qualification report](../quality/cargo-validation-2026-10-26.md).

The v4 host runner validates a trusted project in a disposable candidate. The
developer inspects and approves profiles and policy before enabling execution.
Detecting `Cargo.toml` only suggests presets; it does not enable validation,
replace a custom/older profile, install dependencies or download a toolchain.

## Approved version 2 presets

| Check | Direct command | Default deadline |
|---|---|---|
| `cargo_check` | `cargo check --workspace --all-targets --locked --offline --message-format=json --color=never` | 180 s |
| `cargo_test` | `cargo test --workspace --locked --offline --no-fail-fast --message-format=json --color=never -- --test-threads=2` | 900 s |

The configurable range is 1–900 seconds. Preparation and execution consume the
same check budget. Commands use an executable and argument vector, never a shell
constructed from model output. Dependencies and the selected toolchain must
already be available locally. Offline Cargo mode does not make the trusted
project runner a filesystem or network sandbox.

## Policy and candidate inputs

`CargoValidationPolicyV1` is a versioned host-only sidecar with `schemaVersion: 1`,
explicit relative `additionalInputs`, optional `toolchain`, and `parserVersion`
(default 1). The host freezes policy and profile before dispatching units.
Updating either requires a new approved configuration and invalidates incompatible
evidence; a worker cannot select commands or lower a required check.

Before unit dispatch, the host captures a fenced baseline and complete manifest.
The build candidate overlays validated outputs onto an independent physical copy;
baseline comparison uses another copy. No hardlink may let a build alter the
baseline or live checkout. Resume uses the captured baseline, not a silent
recapture of current files.

Limits are 10,000 files, 64 MiB total and 1 MiB per file. Exceeding a limit is a
preparation error, not a partial snapshot marked complete. VCS metadata, caches,
generated output and unsafe/sensitive paths remain excluded. Explicitly selected
fixtures/assets may be copied as bytes and hashed within these limits; they are
local command inputs and never become LLM context or editable destinations.
Relevant run inputs are checked before and after the command. Unexpected source,
configuration or asset mutation prevents a successful integrity claim.

The host excludes exact generated stage-document paths identified by ADR-032
artifact kinds, using `capture_baseline_excluding` and persisted `excludedPaths`.
It does not exclude legitimate project documents by directory or guessed name.
This prevents a Test report published after execution from making its own
software-validation receipt appear stale. Restore/materialization preserve the
same exclusion set.

`additionalInputs` accepts exact relative paths or `globset` globs: at most 64
selectors of 1,024 bytes each. Empty/absolute paths, drives, UNC, backslashes and
`.`/`..` components are rejected. Every selector must match an allowed file;
the permanent denylist takes precedence. Opaque assets and vendor inputs require
explicit selection. Manifest metadata and its completion marker live beside the
copy in `.<name>.ikore-candidate` (with legacy fallback for `.<name>.hvc-candidate`), outside the command's candidate inputs.

## Evidence and states

The host probes installed cargo/rustc/rustdoc with `--version` and runs
`cargo metadata --locked --offline --format-version=1`. A recognized first
version line is bounded to 512 characters; an unrelated prefix is unknown.
Missing tools, dependencies or preparation configuration are distinct from
software compilation/test failures. No secret value is persisted to identify an
environment; new excerpts pass through sanitization before persistence or export.
Selected environment values detected as secrets cannot qualify the run. Path
dependencies must remain inside the candidate root; external registry/git caches
are an explicit reproducibility limitation rather than a claim of hermeticity.

`ValidationCommandProfileV1` and `ValidationReceiptV1` retain their existing shapes.
Additive migration V55 stores policy and `RunEvidenceDetailsV1` sidecars. Older
rows remain readable and are shown without details that were never collected.
V55 also adds `stage_cargo_validation_contexts`: the captured baseline hash and
frozen profile/policy/tool bindings are durable per execution and reused on
resume, even if project settings changed meanwhile.

| Evidence dimension | Values |
|---|---|
| Phase | `preparation`, `execution` |
| Interpretation | `supported`, `partial`, `uninterpreted`, `not_applicable` |
| Interpreted check result | `passed`, `failed`, `unknown` |
| V1 receipt status | `passed`, `failed`, `timed_out`, `cancelled`, `unavailable`, `not_run`, `error` |

Details include candidate integrity, bounded preparation probe output, observed
tool versions, test totals when available and explicit limitations. Unknown totals
are not zero. A check passes only in the execution phase after successful
metadata preparation, three known tool versions, an actual completed process
with exit 0, supported interpretation, a passed result and preserved candidate
integrity. Tool changes during execution also invalidate qualification.
Unknown/malformed required output is error/unknown even with
exit 0. Truncation that removes necessary evidence cannot produce a green check.

The combined retained stdout/stderr cap is 1 MiB per command, with at most 64
normalized diagnostics. Readers keep draining discarded output. Timeout,
cancellation, incomplete drain and failed process containment do not pass.
These bounded excerpts are not complete logs.

## Freshness and review

Reuse requires the same candidate and relevant input set, effective command and
profile, policy/parser, tools and relevant environment. A stable check name or
unchanged glob list is insufficient. Changed or unverifiable bindings produce a
visible stale/unverifiable result rather than reassigning old success to new work.
Local `.cargo/config` and `.cargo/config.toml` are candidate inputs. The environment
fingerprint also binds known nonsecret `CARGO_HOME/config` and
`CARGO_HOME/config.toml` content (or the resolved local Cargo home). It never reads
credential files. Unsafe, unreadable or secret-bearing configuration cannot
establish comparability; external dependency caches remain an explicit limit.

The Integrator uses ADR-032's existing host evidence, manifest and gap rules.
Its schema 3 manifest adds interpretation, result and limitations; older rows
remain readable with those details absent. Host text is sanitized before both
manifest persistence and prompt rendering. Receipt IDs stay in metadata/UI.
Snapshots, UI and reports must preserve preparation, interpretation, integrity,
truncation and unavailable states. The model cannot turn unknown evidence into a
passed check, and `REVIEW.md` cannot remove a mandatory gate or human checkpoint.
Baseline comparison requires separately executed comparable evidence; a
pre-existing failure remains a failure.

## Qualification boundaries

The acceptance matrix includes real offline workspace check/test runs, assets,
immutable baseline/resume, independent copies, missing tools/cache, budget and
process failures, unsupported output, post-run mutation, individual binding
changes, migration and sanitization. Tests and dated reports must record actual
commands and outcomes. Windows Job Object lifecycle, physical smoke and GPUI
interaction require separate validation; Linux headless results do not establish
those gates.

Security-specific profiles, mutation testing, other ecosystems and spans remain
outside this first increment. Existing risk-based selection belongs to ADR-034;
it consumes this execution evidence without installing or authorizing commands.
An interpreted failed check is executed negative evidence, never an approval.
Partial, unknown or stale qualified evidence remains unverified for coverage.

## Inspect candidate capture

From the Ikore workspace, replace the placeholders with the local project root
and an approved asset selector:

```text
cargo run --locked --offline -p ikore-core --example cargo_candidate_capture -- "<project-root>" "<asset-pattern>"
```

The asset argument is optional and repeatable. A development-environment example
is `-- /workspace/Ikore crates/ikore-app/assets/icon.ico`; the placeholder form also
accepts Windows paths. The system temporary directory must be outside the target
root. `cargo run` may compile the Ikore inspector itself; the example only captures,
restores and verifies the target, without running its build, installing its
dependencies or making network requests. Temporary copies are removed on exit.

JSON output includes the capture-only purpose, restoration/integrity result,
candidate hash and counts of files, bytes, opaque files, baseline/overlay inputs
and selected input categories. These are capture diagnostics, not target
compilation/test results or a passed qualification gate.

## Integration with feature 10

The implementation is integrated on Ikore `9145783` (ADR-034). Migration V55
follows its V52–V54 migrations. Review manifest schema 3 preserves the risk,
selection and Intake-frozen review policy fields introduced in schema 2.
Reconciliation of ordinary project documentation remains part of freshness;
only exact host-owned stage documents are excluded.

For the Ikore capture pilot, approve both `crates/ikore-app/assets/icon.ico` and
`crates/*/fixtures/cli/*/current/hostile.txt`. The latter selects four intentional
NUL-containing test fixtures. Their opaque bytes remain local to the candidate.
