# ADR-037: host-mediated agent exchange

Date: 2026-10-29

Status: accepted; implemented on 2026-10-30, except the ACP transport. M5 ended
in a spike report because its parity gate was not run. Migrations: V62 to V64;
V65 was not needed. See [Implementation](#implementation).

## Context

The host already decides what every agent receives (ADR-022): stages hand over
work through artifacts the host accepted, and no agent addresses another.
Five gaps remain:

- **Provenance per item.** Content from other agents, check output and
  workspace files enters prompts without saying who produced it. Trust is one
  general sentence in the system prompt, so text planted in a file or in a
  test's output carries the same apparent weight as the host's own text.
- **Lossy events.** The application event bus is a bounded broadcast. A
  subscriber that lags reloads everything, a gap in a terminal run's events
  reloads the whole Agents panel, and pulsation progress depends on polling.
- **Resume after edits.** An explicit CLI session resume is refused when the
  workspace snapshot changed, for any edit, and the host cannot say what
  changed.
- **Memory across pulsations.** The Companion can search agent outputs, but
  has no cheap way to answer "why did we decide X in yesterday's pulsation?".
- **CLI transport.** Each CLI has its own stdout parser and permission flags.

## Decision

A host-mediated **agent exchange** layer records, labels and routes every piece
of information that moves between agents or to the UI. Acceptance and
publication stay where they are, and the host remains the only router.

**Provenance envelopes (M1).** Every content item the host did not write gets
one `> provenance:` line under its title, outside its fence, from a typed
`ExchangeItemV1`. There are five trust classes: `host`, `human`,
`agent_output` (with the approval gate when the developer approved it),
`tool_output` and `repository`. The class is derived from the producer, never
passed by a caller. Only `host` and `human` items may instruct a model; a
legend in the system prompt replaces today's trust sentence. Citable titles,
handles and response contracts do not change. Prompt growth is capped and
measured on the fixture benchmark of ADR-022.

**Durable exchange log (M2).** Two additive tables record references, not
copies, in the same transaction as the domain write: pulsation and checkpoint
transitions, unit attempts, publications, terminal run states, finished
Companion turns, Forge, notes, drawings and documentation proposals. Appends
are idempotent and return `recorded`, `duplicate` or `fenced`; a fenced record
aborts the domain write. The event bus carries a doorbell with a global
sequence, a lagging subscriber rereads from its cursor, and a full reload
becomes an explicit `stream_reset`. Terminal output events keep their own
sequence and are not copied. Polling stays as a slower fallback.

The legend that shipped in M1 has 348 characters; per prompt, the fixture
benchmark measured +915 characters at the median on v4 and at most +497 on the
legacy pipeline, within the absolute ceiling (+1,000 median and +1,700 max on
v4, +600 on legacy).

**External changes on resume (M3).** A content change in the workspace no
longer refuses an explicit resume on its own. New sessions record a per-file
snapshot manifest and a fingerprint without the workspace hash. On resume the
host compares the manifests and tells the agent, before the user's message,
which files were modified, created, deleted or renamed, with hunks for small
text files. Above a budget of 40 files or 8,000 characters the resume is still
refused, now with the count. Sessions recorded before this change keep the
strict rule. The Companion warns, with paths only, when a file read in an
earlier turn changed.

**Memory across pulsations (M4).** Each finished pulsation gets a
deterministic, versioned digest derived by the host without a model, indexed
with SQLite FTS5. The Companion gains two read-only tools, `search_pulsations`
and `read_pulsation`, still scoped to the project, sanitized and bounded.

**ACP behind a gate (M5).** The Agent Client Protocol is evaluated as an
additional CLI transport, off by default, only after a spike and a parity gate
on the fixture benchmark. The host is the ACP client: writes, execution and
terminals are refused, file reads come from the safe snapshot, and the JSON
response contract does not change. Adapters are installed by the user and
accepted only from a fixed allowlist of versions at least 14 days old. Making
ACP the default for any backend needs an amendment with the gate's evidence.

**Reviewer contract (M6).** `review_finding` and `claim_under_verification`
are reserved item kinds for a future multi-reviewer feature; independent
reviewers never see each other's envelopes, and approving is not publishing.

## Consequences

Each prompt item states where it came from, so check output and workspace text
no longer read like the host's instructions. The line does not stop a model
from obeying injected text; it makes the boundary explicit, and the main
defense stays the same: no worker writes, and the host accepts and publishes.
Prompts grow by one line per item plus the legend. Every transition write gains
one append in the same transaction, measured against a baseline recorded before
implementation.
Resumable sessions store a per-file manifest and, for hunks, sanitized
projected content capped at 1 MiB per run. ACP adds a Rust dependency and
third-party adapters that release several times a day, hence the allowlist and
supply-chain review. All new tables are additive and are deleted with their
project. Notices and digests pass through secret sanitization (ADR-030) before
reaching a model or the UI. Exchange items are not part of the pulsation
attestation predicate (ADR-036).

## Implementation

| Milestone | Commits | Validation |
|---|---|---|
| M1, provenance envelopes | `99cc886`, `bfb02d2` | [M1](../../quality/agent-exchange-m1-validation-2026-10-30.md) |
| M2, exchange log and doorbell (V62) | `3d7ab74`, `2ddcb7c` | [M2](../../quality/agent-exchange-m2-validation-2026-10-30.md) |
| M3, external changes on resume (V63) | `ab648fc` | [M3 and M4](../../quality/agent-exchange-m3-m4-validation-2026-10-30.md) |
| M4, pulsation digests and two Companion tools (V64) | `d230cda` | [M3 and M4](../../quality/agent-exchange-m3-m4-validation-2026-10-30.md) |
| M5, ACP | none | Spike only; see below |
| M6, reviewer contract and closing | `9526c77` | [Closing](../../quality/agent-exchange-closing-2026-10-30.md) |

Where the implementation differs from the decision:

- The doorbell reads the log every 250 ms instead of being published by each
  writer, so no writer needs the event bus and no doorbell announces an
  uncommitted item.
- Only manual CLI runs, the ones an explicit resume uses, record a manifest.
  The resume state has a table of its own, so V63 changes no existing table.
- Digests have no job of their own: the background lane derives them after
  sealing, and the first search of a project derives the newest 200. A new
  digest version replaces the stored one; the exchange log records each.
- **ACP was not enabled.** The parity gate needs adapters that the user
  installs, and none was installed or run. Reading the adapters' published
  source showed two blockers to resolve first: the Claude adapter loads the
  user's, the project's and local settings (hooks, MCP servers, permission
  rules and the initial mode), so the host's refusals are not the only
  control; and token counts comparable to ADR-020 exist only in each
  adapter's own metadata. No ACP code or dependency was added.
- **Transition latency.** Each pulsation transition gains 0.4 to 0.9 ms at
  p95 in the debug test profile, 12% to 26% over the code before M1 in
  alternating runs on one machine. That is under the 15 ms cap but over the
  10% relative ceiling, which is one of this decision's review criteria. The
  developer accepted the measured cost on 2026-10-30; a later alternating
  measurement above 0.9 ms at p95 reopens the question.
