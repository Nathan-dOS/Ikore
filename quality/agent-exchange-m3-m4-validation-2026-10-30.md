# Agent exchange M3 and M4 validation: resume after edits and pulsation digests (ADR-037)

Date: 2026-10-30. Source: branch `feat/agent-exchange`, M3 at `ab648fc` and
M4 at `d230cda`, gated together on `d230cda`. Compared with the
[M2 validation](agent-exchange-m2-validation-2026-10-30.md).

This report approves no distribution.

## What M3 changed

- **Manifest (V63).** A manual CLI run, the kind an explicit session resume
  uses, records a per-file manifest of its snapshot
  (`terminal_run_snapshot_files`) and, for text files up to 32 KiB, the
  sanitized content it saw (`terminal_snapshot_blobs`, shared per project, at
  most 1 MiB per run). Its session fingerprint is version 2, without the
  workspace hash (`terminal_run_resume_state`). Runs recorded earlier stay
  version 1 and keep the strict rule.
- **Resume.** An explicit resume prepares a new snapshot under the parent's
  policy and compares manifests. Before the developer's message, the agent is
  told which files were modified, created, deleted or renamed, with sanitized
  hunks for small text files; the notice goes through the prompt's
  sanitization. More than 40 changed files, or a notice over 8,000 characters
  even without hunks, refuses the resume with the count.
- **Launch fence.** The child run stores the digest of the change list; the
  launch compares the parent's manifest with the real snapshot again and
  refuses when the list differs.
- **Agents panel.** "Resume session" previews the change count: "N files
  changed since this session; the agent will be told".
- **Companion.** Each successful `read_repository_file` records the file's
  digest (`chat_tool_file_observations`). When a turn starts, files read in
  earlier turns of the conversation that changed since are listed in a host
  notice: paths only, at most 50.

## What M4 changed

- **Digest.** When a pulsation ends, the host derives `PulsationDigestV1`
  from its rows, without a model: header, intent, approved decisions,
  criteria, checkpoints, stages, files, risk and coverage, attestation. Each
  section carries its provenance line. The content is sanitized with a
  receipt and capped at 64 KiB, cutting the file list first.
- **Storage (V64).** `companion_pulsation_digests` (one per pulsation), an
  external-content FTS5 index kept by triggers, and
  `companion_pulsation_paths` for the path filter. All of it goes with the
  pulsation and the project. Each stored digest announces itself in the
  exchange log (`pulsation_digest_ready`).
- **When.** After sealing attestations, the supervisor's background lane
  derives the newest missing digest of a finished pulsation whose seal is not
  pending. The first search of a project derives the missing digests of its
  200 newest finished pulsations. `read_pulsation` derives again before
  reading and writes only when the content changed.
- **Tools.** The Companion has ten read-only tools. `search_pulsations`
  searches digests by words, with optional outcome and exact-path filters, and
  returns at most 20 references. `read_pulsation` reads a digest or one
  section, at most 400 lines or 64 KiB, and only within the conversation's
  project. Both results are `agent_output`.
- **Append path.** The exchange log append reads an existing stream instead of
  rewriting it, and a pulsation transition takes its state version from its
  own update: one statement less each.

## Deviations from the plan

- The resume state lives in a table of its own instead of two new
  `terminal_runs` columns, so V63 changes no existing table and the earlier
  upgrade tests, which require every table definition to stay intact, still
  hold.
- Only manual runs record a manifest; they are the runs an explicit resume
  uses.
- Above 40 files the resume is refused, so the notice never omits files in v1.
- Digests have no job of their own: the background lane, the first search and
  each read derive them.
- A new digest version replaces the stored one; the exchange log keeps one
  item per version and content digest. The derived index adds nothing by
  keeping old copies.
- `deleted` is in the vocabulary and the schema, but v1 does not derive it:
  the publication bases record no removal.

## Fixes after this gate

Covered by the closing gate:

- A digest that cannot be stored no longer blocks the older ones or a search,
  and the background lane does not retry it every second. Sanitization can
  lengthen the text, so the content is cut to the cap again after it.
  `a_digest_that_fails_blocks_neither_older_pulsations_nor_a_search`.
- Orphan snapshot blobs were never removed in production: the cleanup
  existed, but no run deletion called it. Clearing a role's or a slot's
  history and deleting a free-form agent now remove them in the same
  transaction. `blobs_stop_at_one_mebibyte_per_run_and_orphans_go_with_their_runs`
  goes through the history-clearing path.

## Tests added

- Contracts: change list by kind and path, rename rule and tie, notice text,
  budget, canonical list.
- Terminal: manifest and small sanitized blobs, changes with hunks, launch
  fence, refusal at 60 files, 1 MiB cap and orphan blobs, manual runs only,
  version 1 rule.
- Core: a resume notice carries only sanitized hunks; the Companion warns only
  when a file read earlier changed; digest determinism with a golden SHA-256,
  section provenance, running pulsations, the cap, sanitized storage with
  receipt and log item, idempotent refresh, backfill; search and read with
  filters and project scope; a question about a past choice reaches the
  approved decision through the dispatcher and the next round's prompt.
- Upgrades: V62 to V63 and V63 to V64 (additive, cascades, index triggers).

## Stage 7 gate

`scripts/gate-stage7.ps1` passed on `d230cda` in 2,442.3 s, in the second
worktree, incremental over the M2 gate.

| Step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | Passed | 2.0 s |
| Crate dependency boundaries | Passed | 0.2 s |
| `cargo fmt` | Passed | 6.9 s |
| Clippy `-D warnings` | Passed | 62.4 s |
| Rust tests (`cargo test-all`) | 60 targets, 1,978 passed, 0 failed, 23 ignored | 1,689.9 s |
| GPUI release build | Passed | 670.7 s |
| Windows GPUI smoke | Window opened, graceful shutdown, exit 0 | 1.7 s |
| Rust licenses and sources | Passed | 2.1 s |
| Rust advisories | `cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `2295466` | 6.3 s |

Against M2: 22 more passing tests and 1 more ignored one (the opt-in search
measurement). Before the gate, a local run of the `hvc-core` library tests
on every CPU thread failed
`router_withholds_incomplete_stream_and_can_try_the_next_target` once; it
passed three times alone and in the gate, which runs two threads. It does
not touch the exchange code.

## Measurements

On `9526c77`, debug test profile:

- **Search** (`exchange_m4_pulsation_search`, 1,000 digests): `search_pulsations`
  with up to 20 results, p50 9.5 ms and **p95 11.3 ms** (target under 50 ms);
  a section read with `read_pulsation`, p50 11.3 ms and p95 13.6 ms. Deriving
  the 1,000 digests took 9.8 s, about 10 ms each, so the first search of a
  project with 200 finished pulsations and no digests waits about 2 s once.
- **Snapshot manifest** (`exchange_baseline_snapshot_manifest`, this
  repository): 1,025 files, 18.2 MB; 888 text files up to 32 KiB. A manual
  run keeps blobs for 144 of them, 1,048,576 bytes, the full 1 MiB budget
  (M3 takes them in snapshot order). The per-file manifest is about 252 KB of
  rows, against 50 KB for the paths-only JSON. Preparing the snapshot took
  42.7 s in the debug profile, as before.

## Rerunning

```powershell
cargo test --locked -p hvc-contracts --lib external_changes -- --test-threads=2
cargo test --locked -p hvc-terminal --lib resume_changes -- --test-threads=2
cargo test --locked -p hvc-core --lib exchange::digest -- --test-threads=2
cargo test --locked -p hvc-core --lib pulsation -- --test-threads=2
cargo test --locked -p hvc-test-support --test serial_upgrade -- v62_to_v63 v63_to_v64
cargo test --locked -p hvc-core --lib exchange_m4_pulsation_search -- --ignored --nocapture --test-threads=1
```
