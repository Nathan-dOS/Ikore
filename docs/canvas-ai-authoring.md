# Canvas AI authoring

**Status: source published on main, 2026-10-29**, in
[`92f03c4`](https://github.com/Nathan-dOS/Ikore/commit/92f03c43bd73521c9f4ee55c51c697ba84f557a8).
The selected source tree excluded unrelated local voice changes and passed 1,778
tests with zero failures and all nine adopted gate checks. Its 884 source hashes
and Git blobs were verified with zero drift. No task branch or worktree was
created. The [publication record](../quality/canvas-ai-publication-2026-10-29.md)
and [qualification record](../quality/canvas-ai-publication-qualification-2026-10-29.json)
identify the published scope. The earlier 1,782-test result remains evidence of
the combined local checkout, as recorded in the
[historical workflow report](../quality/canvas-ai-workflow-validation-2026-10-28.md).
Technical qualification does not certify authenticated providers, physical
display/DPI/IME behavior or a production model's diagram quality.

The Companion can offer a new Canvas explanation after completing its chat answer.
The developer approves that specific offer, generation runs in hidden staging, and
the originating answer receives an **Open Canvas** button after publication.
Completion does not navigate or replace the answer. The developer can edit the
published semantic drawing using the native editor.

## Authority and source

The normal chat runner retains its read-only repository dispatcher. Its terminal
`complete_answer_with_canvas_offer` control carries a final answer and bounded
offer metadata; the equivalent CLI envelope is `final_with_canvas_offer`. Neither
control grants permission or runs a drawing tool. The host records the proposal
only after the assistant answer is persisted as completed.

`CanvasAuthoringStore::propose` resolves that assistant message inside the specified
project and conversation. It stores the sanitized explanation snapshot, content
SHA-256, profile, language, title and request text. Each source answer has at most
one request. Admission and publication recheck the source's identity, finality and
hash. Missing or changed sources cannot authorize a new drawing.

The pure `CanvasAuthoringSource` contract supports an optional opaque document
identity/revision. The runtime path currently captures the completed chat answer
with no document reference; it does not expose a document resolver or let a model
open a source path through authoring tools.

Only the native approval action calls `approve_canvas_request`, with the request's
expected revision, a host action ID and the current Companion selection. Text such
as "yes" in a message, document or model result does not approve a request. A
host-owned job ID and nonce bind subsequent work to that project, conversation and
request. One running Canvas job is admitted per conversation. Decline and Stop
preserve the answer and publish no unfinished drawing.

Conversation loading returns at most the 1,000 most recent offers in chronological
order, including deterministic insertion order for equal timestamps. Older request
history remains in SQLite; this limit does not delete offers or increase job budgets.

At admission the host freezes the Companion target. API jobs record connection,
provider, model, connection version, protocol and configuration fingerprints;
credentials and endpoints are not stored in the target metadata. The captured
configuration is checked before rounds and publication. CLI jobs freeze backend,
model selection/source and applicable profile. Later conversation selection
changes do not retarget an admitted job. Unavailable or changed captured targets
fail without a provider/model fallback.

## Scoped tool loop

The job uses a separate, serialized tool controller. API payloads use native
function schemas for Responses, Chat-compatible and Anthropic Messages protocols.
Supported CLIs return a closed control envelope through the existing isolated
Companion runner; their native filesystem, shell, web, MCP and other tools remain
disabled. No repository read dispatcher is added to this drawing loop.

| Tool | Host behavior |
|---|---|
| `canvas_inspect` | Return this job's staged scene, revision and diagnostics. |
| `canvas_apply_operations` | Validate and atomically apply a bounded batch at the expected staged revision. |
| `canvas_validate` | Return profile/completeness diagnostics without publishing. |
| `canvas_finish` | Validate the exact staged candidate and request one publication. |

The wire operations are `upsert_node`, `connect`, `remove` and `set_narrative`.
The host compiles them into the shared core command reducer and owns visual
defaults, scope and provenance. Profile, language and source provenance cannot be
replaced through model operations. Invalid batches leave the previous scene
unchanged. Operation IDs make equal retries idempotent; reusing an ID for different
content is rejected. Plain final prose cannot publish, and `canvas_finish` must
be the last call in its round.

| Boundary | Current limit |
|---|---|
| Runtime job | 5 minutes |
| Model rounds / total tool calls | 8 / 24 |
| Calls returned per round | 1–4 |
| Aggregate tool results | 256 KiB |
| Batch / encoded batch | 128 operations / 256 KiB |
| Pure session's distinct admitted batches | 128 |
| Host grant lifetime | At most 15 minutes; the worker still has its 5-minute limit |
| Source explanation | 64 KiB and 16,000 characters |
| Narrative / title | 4,000 / 120 characters |
| Native scene | 1,000 elements and 4 MiB encoded scene |

CLI rounds retain the existing 120-second round deadline. These are independent
authoring bounds, not an increase to ordinary chat budgets. Generated labels,
titles and narrative preserve the admitted conversation language; this local
Canvas exception does not change pipeline artifact or technical documentation
language rules. Recognized secrets are sanitized at the established host
boundaries before durable storage and export.

## Persistence, publication and recovery

The published source includes `V58__canvas_authoring_jobs.sql`. V57 and earlier
applied migrations are unchanged. Future integrations must use free versions
above V58 for their new migrations and reconcile against the actual merged
history before applying them; V58 is occupied by Canvas authoring.

`companion_canvas_requests` owns the durable lifecycle:
`awaiting_authorization` becomes `declined` or `running`; a running request ends
as `ready`, `failed`, `cancelled` or `interrupted`. Staged scene revisions are
separate from drawing content revisions. Staging is absent from the drawing
library and never appears progressively on the native surface.

Publication holds the shared project guard and a SQLite transaction. It rechecks
the worker nonce, authorization/expiry, frozen target binding, source hash and
exact recorded candidate, then inserts the complete initial drawing, its mirror
intention and the ready receipt together. There is no visible empty placeholder.
Repeated completion of the same candidate returns the same receipt. Cancellation
before the commit prevents publication; a completed commit remains ready when a
late cancellation arrives. Project deletion temporarily fences new admissions
without changing offers. A running Canvas job must be stopped before deletion,
matching other active work. Rejected deletion preserves requests/state; accepted
deletion cascades them. The admission fence is released on every return path.

SQLite remains authoritative. A ready drawing can have pending/conflicting
JSON/SVG mirrors under `.hvc/drawings/`; the existing recoverable sync journal
preserves divergent external files. Ready does not certify synchronized files.
The receipt fixes project, conversation, request, job, drawing ID and initial
content revision. Opening resolves that receipt and rechecks current availability,
rather than using the selected drawing. Delayed UI callbacks cannot navigate a
different project or override subsequent navigation.

Startup marks running requests interrupted and clears their staged scenes; it
does not resume model calls or publish partial work. Failure/cancellation also
clears staged content. Request metadata and source snapshots remain for the
conversation's lifetime. Deleting an individual source answer or result drawing
leaves an explicit unavailable-source/result tombstone; replay cannot recreate a
deleted drawing. Deleting the conversation/project cascades its requests.

The durable record contains source content and authorization/target/publication
metadata and hashes; it is not a content-free audit. Model usage is recorded in
the existing independent ledger with `source_kind = canvas_model_call`. There is
no new durable transcript of every intermediate tool result or a promise to
resume a partially executed tool loop after restart. Lifecycle events carry
identities/revisions, and UI hydration reloads the authoritative request list.

## Semantic editing

`CanvasSceneV1.document` is an optional, separately versioned semantic document.
Legacy scenes remain untyped; missing semantics are not inferred from colors,
positions or containment. Current readers accept legacy scenes. Older closed
readers can reject the added document field; forward compatibility is not claimed.

The architecture profile supports components, boundaries, relationships,
annotations and associations. The restricted Ikore process profile supports generic,
user and service tasks, none start/end events, exclusive/parallel gateways,
participants, lanes, sequence/message flows, annotations and associations. It
does not claim full BPMN conformance, execution or BPMN XML interchange.

Manual type, membership and endpoint controls use the same core validation as
authoring. Sequence flow stays within its participant; message flow crosses
participants with eligible endpoints; starts cannot have incoming sequence flow
and ends cannot have outgoing sequence flow. Geometry changes do not silently
change participant/lane membership. Rejected edits restore the previous scene.
Undo/Redo snapshots include semantics with geometry, preserving the existing
100-snapshot/32-MiB history bounds. Save/reopen retains the semantic document.
Provenance remains attached while later manual commits advance drawing revisions;
the ready receipt retains the generated baseline revision.

The native explanation panel shows narrative and source identity. Draft
completeness diagnostics are separate from invalid topology and are not proof of
architectural accuracy, process correctness or executability. A structurally valid
candidate may retain warnings. The native panel shows their total count, the first
three messages with bounded element labels and an additional-warning count.
Diagnostics are derived from the scene on load, accepted edits, Undo/Redo and save
acknowledgement; they are not recomputed while painting or stored in another table.

Rendering remains GPUI/Rust, core remains independent of GPUI, and SQLite/filesystem
work stays off the graphics thread. Authoring grants only this new Canvas scene
and its managed persistence. It grants no source editing, general workspace write,
shell, Git, settings mutation, external MCP or executable business process.

Implementation entry points: [job runtime](../crates/ikore-core/src/chat_service/canvas_authoring.rs),
[durable store](../crates/ikore-core/src/companion_drawings/authoring_store.rs),
[pure controller](../crates/ikore-core/src/companion_drawings/authoring.rs),
[semantic contracts](../crates/ikore-core/src/companion_drawings/semantics.rs), and
[consent/result UI](../crates/ikore-ui/src/canvas_authoring_ui.rs).
See [testing](testing.md#canvas-ai-authoring-candidate) for focused selections.
