# Signed local attestation (ADR-036 M3) — implementation and validation

Date: 2026-10-26. Branch `feat/mandates-layers-attestation`, implementation
`66acb79`, `dc1a418` and `1512707` on top of the M2 commits (`b14f0f7`, `0343818`, see the
[M2 report](mandates-attestation-m2-validation-2026-10-26.md)). Decision:
[ADR-036](../docs/adr/036-pulsation-attestation.md). Format, key and the
OpenSSL steps: [attestations](../docs/attestation.md). Migration V56. Nothing
is pushed.

This report qualifies sealing, signing, verification, export and their
display against hermetic fixtures (scripted model gateway, temporary
workspaces and databases, a credential store in memory). No real credentials,
CLI sessions or paid model calls were used, and no test touched the Windows
Credential Manager. GitHub Actions was not used and is not evidence here.

## Delivered behavior

- **Statement.** An in-toto Statement v1 (`urn:hvc:pulsation-attestation:v1`)
  whose subjects are the paths the pulsation's main publications applied, with
  the applied SHA-256 (the last applied publication of a path counts). The
  predicate has eleven blocks (`pulsation`, `builder`, `intent`, `authority`,
  `execution`, `evidence`, `sanitization`, `effects`, `conformance`, `usage`,
  `sealing`) holding identifiers, digests, enumerated states, relative paths
  and integers only; a reason is kept as its digest. Canonical JSON (sorted
  keys, no whitespace) is signed in a DSSE v1 envelope with Ed25519 (`ring`).
- **Key.** One Ed25519 key per installation, PKCS#8 in base64 in the
  operating system's credential store (service `hard-vibe-code.runtime`, entry
  `attestation-signing-key-v1`); public keys in `attestation_keys`. The first
  seal creates the key. A missing or replaced key is reported (`key_missing`,
  `mismatch`) and never replaced silently: "Create a new key" in Settings →
  Security asks for confirmation, retires the active key and keeps its public
  key, so earlier attestations still verify.
- **Sealing.** The terminal transition records a seal intent in its own
  transaction (`skipped` when sealing is off). The background lane seals the
  oldest pending intent once the pulsation's main publications are neither
  pending nor applying, in one immediate transaction: key, derivation,
  signature and the immutable row. Without a usable key the statement is kept
  `unsigned` and can be signed later; over 1 MiB only its digest is kept
  (`too_large`); a derivation the host cannot complete is kept `failed` with
  its error until "Seal again". A trigger refuses every update except
  unsigned → signed with every other column kept.
- **Verification.** Signature (`valid`, `invalid`, `unsigned`, `unknown_key`,
  `too_large`); re-derivation from the current rows with the builder and seal
  time the statement recorded (`matches`, `differs` with the first block,
  `unavailable`); and each subject in the workspace now (`unchanged`,
  `modified` shown as "changed since the attestation", `missing`,
  `unreadable`). The last check is cached for display only.
- **Export.** `<pulsation>.intoto.jsonl` and
  `hvc-attestation-key-<8 hex>.pub.pem`; a commit trailer
  (`HVC-Pulsation`, `HVC-Attestation: sha256:<statement digest>`). "Verify a
  file…" checks a bundle against this machine's keys or the public key chosen
  with it and says whether the key is known here.
- **Display.** An Attestation card on the pulsation Overview, below the
  Layers panel (Verify again, Save attestation…, Copy commit trailer, Sign
  now, Seal again, by state); an "attested" mark in the pulsation list; a
  Settings → Security page (key state and keys, create or rotate with
  confirmation, the sealing switch, Verify a file…); a Provenance section in
  the report; the project deletion page says attestations go with the project.

## Acceptance matrix

| Criterion (implementation plan) | Executed evidence |
|---|---|
| Same rows, same statement; golden vector with a fixed test key | `attestation::tests::the_test_vector_is_stable` (statement, digest, envelope, public key, PAE and signature fixtures); `the_deriver_reads_each_block_from_its_rows` derives twice and compares bytes; every seal is re-derived to `matches` in the persistence and integration tests |
| A changed linked row makes the re-derivation differ | `tampering_with_rows_files_or_the_envelope_is_detected` (a late enforcement event → `differs` in `conformance`; a changed applied digest → `subject`) |
| A changed envelope invalidates the signature | `any_change_to_the_envelope_breaks_the_signature` (payload, signature, payload type, another key); `tampering_with_rows_files_or_the_envelope_is_detected` (statement and payload edited with the trigger dropped); `an_exported_attestation_verifies_here_and_elsewhere` (an edited bundle) |
| Editing a subject changes only the workspace level | `tampering_with_rows_files_or_the_envelope_is_detected` (edited → `modified`, deleted → `missing`, signature valid, re-derivation matches); `a_composed_pulsation_that_finishes_is_sealed_signed_and_verifiable` |
| Key material never in database, log, event, report or export | `the_statement_carries_no_content_names_or_key_material` scans every column of every table for the stored secret, and the bundle, the PEM, the export notice and the key status; `a_key_round_trips_through_its_stored_secret_and_never_prints_it` (the key's `Debug` shows only its id). No log or event carries key material by construction: nothing in the sealing path writes either |
| Golden vector verified by OpenSSL 3 following `docs/attestation.md` | OpenSSL 3.2.3: `Signature Verified Successfully` on the vector; the documented script reproduces `pae.bin`, `sig.bin` and `statement.json` byte for byte; the key id and the statement digest match. The same steps verify a bundle exported by the integration test (below) |
| Unavailable keyring → visible `unsigned`, signable later, no crash | `without_a_keyring_the_statement_is_kept_unsigned_and_signed_later`; `a_missing_key_is_never_replaced_without_asking`; `a_replaced_key_is_a_mismatch_until_a_new_key_is_created` |
| A pulsation that ends `done` is sealed | `a_finished_pulsation_is_sealed_once_and_signed_with_a_first_key`; `attestation::a_composed_pulsation_that_finishes_is_sealed_signed_and_verifiable` (full v4 pipeline: subjects equal the applied publications, mandate, receipts, attempts with observed models, `withinMandate`, valid, matches, unchanged, Provenance in the report) |
| A failure with effects is sealed with `outcome: failed` | `a_pulsation_aborted_after_writing_is_sealed_as_failed_with_its_effects` (Code parks, the developer aborts, the earlier stages' notes are the subjects); `the_outcome_follows_how_the_pulsation_ended` (`failed`, `rejected`, `aborted`) |
| A crash between the terminal transition and the seal resumes | `a_restart_between_the_end_and_the_seal_resumes_it` (a new service on the same data directory seals the pending intent); `a_database_error_other_than_busy_leaves_the_queue` (a busy database leaves the intent pending, any other error marks it failed) |
| A pending publication delays the seal | `sealing_waits_for_the_pulsations_main_publications` (`pending` and `applying` wait, `applied` seals) |
| Tampering with database, envelope and workspace | See the three rows above |
| Export and import | `an_exported_attestation_verifies_here_and_elsewhere` (file names, known key here, unknown key with the PEM elsewhere, no key, edited bundle, garbage, trailer); the integration test verifies a bundle in a second data directory |
| Project deletion removes the attestation, keeps the keys | `deletes_one_repository_and_its_history_but_retains_attributed_usage` (intents, attestations and checks gone, keys kept) |
| Upgrade V55 → V56 without retroactive sealing | `v55_to_v56_adds_attestations_without_sealing_pulsations_that_already_ended` (nothing rebuilt, a pulsation that ended before stays unsealed, every CHECK, the one-active index, the trigger, the key foreign key, cascades) |
| Content exclusions | `the_statement_carries_no_content_names_or_key_material` (file content, a risk reason, the feature name and the workspace path are absent; the reason appears as its digest) |
| Size cap | `a_statement_over_the_size_cap_is_kept_as_a_digest` (3,000 subjects → `too_large`, re-derivation matches, not exportable) |
| Rotation state machine | `rotation_keeps_one_active_key_and_every_public_key`; `a_missing_key_is_never_replaced_without_asking` |
| Immutability triggers | `an_attestation_never_changes_except_to_be_signed`; the V56 upgrade test |
| Windows, opt-in: Credential Manager creation, reading and rotation | `attestation::the_credential_manager_keeps_creates_and_rotates_the_key`, ignored by default and **not run** for this report (see Limits) |
| UI: card states, key page, report golden | `the_attestation_card_shows_a_signed_record_and_what_changed_since` (960 and 1440 px, a 260-character path, eight paths then the count, the list mark, the trailer on the clipboard); `the_attestation_card_offers_what_each_state_allows`; `the_security_page_shows_the_key_and_asks_before_a_new_one`; `the_provenance_section_repeats_the_attestation_and_older_reports_still_parse`; `rendering_is_pure_and_matches_the_golden_file` with the regenerated golden (the diff only adds the navigation entry and the section) |

## Findings during the implementation

- **Unit tests could reach the real credential store.** Several `hvc-core` unit
  tests build services on the default store, and one runs the worker loop. Once
  sealing exists, a pulsation ending in such a test would have read the
  developer's Windows Credential Manager and could have stored a key there.
  Under `cfg(test)` the keyring store now reads as empty and refuses writes;
  tests that need a key inject a memory store. Integration tests already used
  `MemoryCredentialStore`.
- **A broken seal could hold the background lane.** The first version
  retried every database error and propagated it out of the lane, so a query
  that cannot run (a schema defect) would have been retried every second and
  kept learning, anticipation and visualization from running. Only
  `SQLITE_BUSY` and `SQLITE_LOCKED` are retried now; any other error marks the
  intent `failed` with its message, and a postponed seal no longer stops the
  rest of the lane (`a_database_error_other_than_busy_leaves_the_queue`).
- **Line endings of the vector.** With `core.autocrlf`, the envelope and the
  PEM fixtures would be checked out with CRLF and fail the byte comparison;
  the fixtures folder is `-text` now (`dc1a418`).
- **"Verify a file" hid behind the key status.** The settings search test
  found the section missing when the key status could not be read; checking a
  saved bundle needs no key of this machine, so the section always renders now
  (`1512707`).
- **Relative paths can carry the feature name.** The exported bundle of the
  integration test lists `knowledge/obsidian/features/token-expiry.md`: a stage
  note is named after the feature. Paths are kept as they are, and the
  documentation and the card say so.

## Validation results

Environment: Windows 11 Pro 10.0.26300, Rust/Cargo 1.98.0, `--locked`,
`CARGO_BUILD_JOBS=6`, in the branch's own worktree.

The Stage 7 gate (`scripts/gate-stage7.ps1`) on `1512707` (2026-10-26
23:05–23:34 local time, 1,708 s) **passed**:

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 0.736 s |
| crate dependency boundaries | PASS | 0.134 s |
| cargo fmt | PASS | 4.751 s |
| Clippy (`-D warnings`) | PASS | 1.512 s |
| Rust tests: 55 targets, **1,648 passed, 0 failed, 17 ignored** | PASS | 1,173.406 s |
| GPUI release build | PASS | 519.704 s |
| Windows GPUI smoke (window opened, graceful shutdown, exit 0) | PASS | 1.758 s |
| Rust licenses and sources (560 packages for `x86_64-pc-windows-msvc`, none denied) | PASS | 1.697 s |
| Rust advisories (`cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `3279c99`) | PASS | 4.841 s |

Against the M2 gate (`b14f0f7`: 54 targets, 1,617 passed, 16 ignored): one
more target (`attestation`) and 31 more passing tests, all of them new (6 in
`attestation`, 18 in `attestation_persistence`, 1 report test, 2 in
`tests/attestation.rs`, 1 upgrade test and 3 layout tests); the extra ignored
test is the opt-in Credential Manager test. Two earlier gate runs on this
branch stopped at Clippy (`result_large_err` in that test's helper) and at the
settings search test (the "Verify a file" section, `1512707`); both are fixed
in the commits above. The supply-chain summary still reports
`distributionAllowed: false` for the existing GPUI binary blocker.

`scripts/benchmark-ui-latency.ps1 -EnforceThresholds` on the gate's release
binary (idle scenario, 120 samples after 20 warm-up samples): frame-callback
interval p95 6.86 ms and p99 7.47 ms against the 8.33 ms and 16.67 ms
thresholds, **passed**. The scenario opens the application without a
pulsation, so it shows that the shell did not get slower, not the cost of a
seal. Sealing has no cost target in the plan and was not timed.

OpenSSL on a real bundle: with `HVC_ATTESTATION_OUT` set, the integration test
saved the finished pulsation's bundle and key; the script in
`docs/attestation.md` extracted the PAE and signature, and
`openssl pkeyutl -verify -rawin` printed `Signature Verified Successfully`. The
statement had four subjects and the eleven predicate blocks; the key id equals
the SHA-256 of the PEM's last 32 bytes.

Dependencies: `ring`, `base64` and `zeroize` were already in the graph through
rustls; `keyring` was already a dependency of `hvc-core` and is now also a
dev-dependency of `hvc-test-support`. `Cargo.lock` gains only those dependency
edges, no new package.

## Limits

- Hermetic fixtures only. No real CLI or provider run, and no Linux headless
  run of this branch.
- The opt-in Credential Manager test was not run: it writes to the real
  Windows store (under a test-only service name, removed afterwards), which
  needs the developer's go-ahead.
- The card, the mark and the Security page are covered by GPUI layout tests,
  not by a manual visual pass in the running application; that pass belongs
  to M5.
- A valid signature proves integrity since sealing and the local key, not a
  person, a trusted time, a hermetic build or the absence of another process
  running as the same user. The key in the credential store is readable by
  any process of that user.
