# Public design summaries

These summaries explain the implemented boundaries needed to read and
contribute to this repository. They are curated from the design records and
the source at `bee361b` (2026-10-17). They do not publish the maintainers'
entire design archive or treat proposals as delivered behavior.

Historical report identifiers remain unchanged. Follow their local evidence
links for the checks actually executed at each date.

## ADR-033: Cargo candidate validation

The [accepted design](033-cargo-candidate-validation.md) extends the existing v4
runner with explicitly approved offline workspace profiles, a baseline captured
before dispatch, local asset inputs, preparation/interpretation evidence and full
receipt freshness bindings. It preserves the V1 DTOs through host-only sidecars.
See [Cargo validation](../cargo-validation.md) for the approved commands and
limits. The [integrated verification report](../../quality/cargo-validation-2026-10-26.md)
records automated results and the remaining Windows/GPUI qualification.

## ADR-006: native GPUI application

The desktop application uses Rust and GPUI in one process, with typed core/UI
calls. Only GPUI is vendored; the project does not reuse Zed's editor crates.
The [workspace overview](../architecture.md) and
[GPUI patch ledger](../../third_party/gpui/HVC-PATCHES.md) describe the boundary.

## ADR-007: synchronization and preparation

Pulsations use versioned inputs, explicit stage handoff, project ownership,
human checkpoints and recoverable publication. Stale or cancelled work must
not publish late output. Preparation and background work do not grant a
second owner of the main project execution lane.

The historical implementation sequence is recorded in
[M0](../../quality/pulsation-sync-m0.md), [M1](../../quality/pulsation-sync-m1.md),
[M2](../../quality/pulsation-sync-m2.md), [M3](../../quality/pulsation-sync-m3.md),
[M4](../../quality/pulsation-sync-m4.md) and [M5](../../quality/pulsation-sync-m5.md).

## ADR-008: context telemetry

The host records considered/delivered context and validates citations against
delivered paths. Missing citations alone do not prove that context was useless.
Learning is a background, consultative job. It may select host-supplied graph
candidates; human approval, expected hashes and host publication checks remain
required for changes. Observe is the default mode.

See [implementation and validation](../../quality/learning-context-optimization.md)
and the [learning service](../../crates/hvc-core/src/orchestration/learning.rs).

## ADR-010: CLI runtime

CLI adapters declare capabilities and parse versioned event streams. Processes
use direct executable/argument specifications, reduced environments, deadlines,
bounded output, cancellation and redaction before persisted events. API
fallback requires an explicit human decision. Model identity and allowed
operations remain host-validated.

See the [threat model](../../quality/cli-threat-model.md) and
[runtime implementation](../../crates/hvc-core/src/terminals.rs).

## ADR-013: generated artifact language

Model-authored structured prose and artifacts must be English. The host
validates that policy before acceptance/publication. Host-rendered documents
may quote developer-owned input verbatim; this does not grant a model an
exception from its output contract. UI text is also English. ADR-029 makes one
narrow exception: a Companion note follows its author's language. Canvas
authoring also preserves the conversation language in its
Canvas-owned title, labels, narrative and internal source explanation. Host UI,
schemas and other generated artifacts retain the English policy. The separate
consent-bound authoring runner grants only four Canvas operations; the ordinary
Companion repository dispatcher remains read-only. See [Canvas AI authoring](../canvas-ai-authoring.md)
and the [source publication record](../../quality/canvas-ai-publication-2026-10-29.md)
for the qualified main commit and its remaining qualification limits.

## ADR-015: navigation

The accepted navigation groups views under Agent, Engine and Companion, with
Settings available separately. Restoring persisted `active_view` determines the
header tab; internal sections and per-tab history are held in memory.

Review/document placement, resizing, input handling and Forge behavior were
extended after the initial navigation decision. Public evidence:
[tabs](../../quality/tabs-ui-validation-2026-09-05.md),
[Companion input](../../quality/companion-input-validation-2026-09-07.md),
[UI interactions](../../quality/ui-interactions-validation-2026-09-07.md),
[Forge composer](../../quality/forge-composer-validation-2026-09-08.md) and
[Pulsations/Workspace](../../quality/pulsations-workspace-validation-2026-09-09.md).

## ADR-016: Forge analysis authority

Developer-started triage may approve eligible scope/create tasks, and dependency
analysis may apply agent-origin edges. The host preserves human relations,
checks cycles and retains explicit dispatch and final acceptance. This
supersedes the earlier advisory-only description.
See [synchronization evidence](../../quality/cerebro-sync-validation-2026-09-09.md).

## ADR-021: stage failure checkpoints

The host classifies supported failures and persists the actions it permits.
Resolution validates checkpoint identity, expected state version and producer
state. Retry does not renew a v4 stage's shared budget; revision invalidates
affected downstream work; Abort is terminal. The UI cannot widen the persisted
decision set. See the [failure service](../../crates/hvc-core/src/orchestration/failure.rs)
and [synchronization evidence](../../quality/cerebro-sync-validation-2026-09-09.md).

## ADR-022: stage composition

See the [public contract map and invariant registry](022-stage-composition.md).

## ADR-027: Review Brief and report

The Brief is derived from validated typed payloads and host tables under the
current checkpoint's read transaction. Markdown remains the persisted review
record. Confirm/Flag and revision diffs assist review; the host still admits
the decision. Visualizer notes are non-normative. The end-of-pulsation report
is rendered by the host, with lifecycle cleanup tied to its pulsation.

Implementation: [review brief](../../crates/hvc-core/src/orchestration/review_brief/mod.rs)
and [reports](../../crates/hvc-core/src/reports/mod.rs).
Integration evidence: [2026-10-17 report](../../quality/main-merge-validation-2026-10-17.md).

## ADR-028: plan-quota meter

The host shows how much of each subscription CLI's 5-hour and weekly windows is
used. Sources are tried in order: official or local channels first (Codex
`app-server`, Codex rollout records, Claude run events, Antigravity `/usage`).
For Claude and Codex the host may also read the CLI's own access token from the
CLI's default credential file and query the provider's usage endpoint. This is a
narrow exception to the keyring-only secret rule: the token stays in memory, is
sent only to fixed provider hosts, is never refreshed or written back, and never
reaches agents, logs, events or the database. Responses are validated and the
meter fails closed; it never shows a guessed number. A one-time notice
names the file read and the host contacted, and a kill switch
(`HVC_DISABLE_QUOTA_ENDPOINTS=1`) and a per-CLI setting turn the credential read
off; a build without the `quota-credential-collectors` feature has no credential
reader at all. A pre-flight check before each stage is advisory and uses the
existing human checkpoint; it never switches provider by itself.

See the [threat model](../../quality/cli-threat-model.md) and the
[quota module](../../crates/hvc-core/src/quota/mod.rs).

## ADR-029: Companion notes

The Notes view (Companion group, `ctrl-9`) keeps loose developer thoughts. A
capture is stored, with secrets redacted, before any model is asked, so a failed
or cancelled answer never loses it. The Notes Scribe, a canonical agent role
routed like the others, turns it into a note (title, summary, Markdown topics,
tags from a closed catalogue) and rewrites it on request. It has no tools, no
repository and no workspace; CLI runs are Companion-hardened and refuse the
Custom backend. Every rule in its prompt has a host check, and a rejected answer
gets one corrective call. Notes follow the developer's language, a narrow
exception to ADR-013: they are working memos that no agent consumes. The Scribe
never creates or changes tasks, features or pulsations.

Voice dictation (phases 2–3) is local. The maintainer's 2026-10-29 amendment
keeps Nemotron 3.5 ASR Streaming 0.6B with NeMo-Speech.cpp 0.2.0 CPU and
replaces Python Whisper medium with Whisper large-v3-turbo Q5_0 through
whisper.cpp 1.9.5 CPU. Whisper fixes Portuguese (`pt`) and transcription;
Nemotron retains automatic language detection. Neither backend requires a
resident speech service, and the Whisper path requires no Python or PyTorch.

The selected backend persists across restarts; both supported installations
coexist and model changes wait for setup or dictation to finish. Runtime
archives and model files are pinned by URL, version or revision, size and
SHA-256. A global push-to-talk shortcut (default `Ctrl+Shift+F9`) records while
held, with a start earcon; on release one short-lived process transcribes the
recording and the text becomes a voice note in the open project, through the
same capture path as typed text. Audio never outlives the transcription, and
holds shorter than half a second are discarded. Windows only. The
[2026-10-29 native Whisper report](../../quality/whisper-cpp-validation-2026-10-29.md)
records current qualification; the
[2026-10-26 medium report](../../quality/whisper-medium-validation-2026-10-26.md)
remains historical evidence for the replaced Python backend.

See the [notes module](../../crates/hvc-core/src/companion_notes/mod.rs), the
[voice service](../../crates/hvc-core/src/voice.rs) and the
[threat model](../../quality/cli-threat-model.md).

## ADR-030: deterministic secret sanitization

The host prepares model context and CLI snapshots independently of the original
bytes used for exact edits. Local, occurrence-bound synthetic-fixture
authorizations retain source provenance through delivery; no global value
allowlist is used. Text becomes observable only after completion and
sanitization. Publication checks protected regions, introduced placeholders and
durable evidence under the current policy, including after restart.

This boundary covers host-provided content and a finite detector catalog, while
native CLI tools remain available. See [secret sanitization](../secret-sanitization.md)
and the [validation report](../../quality/secret-sanitization-validation-2026-10-22.md).

## ADR-031: terminal and shared-contract boundaries

CLI services, repository, adapters, processes and subscription quota belong to
`hvc-terminal`; shared values belong to `hvc-contracts`. Core retains host
authorization, orchestration, migrations and compatibility facades. See the
[decision and validation requirements](031-terminal-boundary.md). This decision
was first numbered ADR-030; its 2026-10-23 report keeps that label.

## ADR-032: Integrator evidence boundary, review policy and convergence

The composed Integrator receives classified inputs: candidate files with a
sanitized base-to-candidate diff, baseline context, author claims labelled as
narrative, structured host evidence and explicit evidence gaps. Gaps downgrade
`supported` verdicts and never promote one. Stage documents are neither
delivered nor editable, and a CLI reviewer reads host-selected context only.
`REVIEW.md` is a read-only, budgeted review policy that no stage may publish.
Human revision rounds beyond a configurable threshold need an explicit decision.
See the [decision](032-integrator-review-boundary.md) and the
[review policy](../review-policy.md).

## ADR-034: tool policy, risk scrutiny, pre-flight guardian and documentation reconciliation

Every composed executor's tool policy is the context scope the transport
enforces. In pipeline v4 a versioned risk policy selects the required checks,
orders the Integrator's diff, captures `REVIEW.md` at Intake and records a
coverage matrix; approving a high level with pending items needs a reason.
Forge triage compares the feature with the project's accepted decisions, and a
blocking conflict holds the approval until the developer decides. Notes linked
to code through `source-files`/`test-files` show when that code changed, and a
tool-less reconciler proposes the note update only on request. Linux headless
validation is done; Windows acceptance is pending. See the
[decision](034-risk-preflight-and-doc-reconciliation.md), the
[review policy](../review-policy.md) and
`quality/feature-10-remaining-validation-2026-10-25.md`.

## ADR-035: execution mandates, scope grants and trust layers

Accepted on 2026-10-26. M1 is implemented (V59): approving a v4 Specification
issues an immutable mandate that records the approved intent, write envelope,
executor capabilities, transports, checks and frozen budget parameters. The
brief shows it before the click ("What approving grants"), stage writes and
dispatches are checked against it, and every checkpoint resolution records a
receipt with the reviewed digests. M2 is implemented too: a read-only Layers
panel on top of the pulsation detail derives Identity, Scope, Guardrails and
Intent from the host's records, the Contract, Test and Integrator briefs show
the four states, the header marks a layer that needs a look, and the report
gains a Layers section. M4 is implemented too (V61): a Code or Test unit
that must change a delivered file it does not own fails with
`scope_insufficient` and names it; the host derives which files it may be
granted, and the failure checkpoint shows a "Scope request" card with
"Grant and retry", which issues a `scope_grant` mandate revision and runs the
existing Retry on the shared budget. M5 closed the feature with its threat
model (`quality/cli-threat-model.md`), the governance block of the post-run
analysis and a visual pass in the running app. See the
[decision](035-execution-mandates.md).

## ADR-036: signed local pulsation attestation

Accepted on 2026-10-26 and implemented (M3, V60). Every pulsation that ends is
sealed as an in-toto statement in a DSSE envelope, signed with an Ed25519 key
kept in the operating system's credential store. The statement binds intent,
approvals, host receipts and applied files without carrying any content. The
pulsation detail's Attestation card verifies the signature, re-derives the
statement and reports each file's state now, saves the bundle with its public
key and copies a commit trailer; the list marks attested pulsations, Settings →
Security rotates the key, switches sealing and verifies a saved file, and the
report gains a Provenance section. It claims no SLSA level, trusted time or
personal identity. See the [decision](036-pulsation-attestation.md) and
[attestations](../attestation.md).

## ADR-037: host-mediated agent exchange

Accepted on 2026-10-29; being implemented. Every content item the host did not
write carries a provenance line with one of five trust classes, and only host
and human items may instruct a model (M1, done). A durable exchange log,
written in the same transaction as each domain change, will let a lagging
event subscriber reread from its cursor instead of reloading everything. An
explicit CLI resume after workspace edits will be allowed with a notice of what
changed, within a budget. The Companion will gain two read-only tools over
deterministic pulsation digests, and the Agent Client Protocol will be
evaluated as an off-by-default CLI transport behind a parity gate. See the
[decision](037-agent-exchange.md).
