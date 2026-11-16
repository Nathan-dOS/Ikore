# ADR-040: agent console and bottom dock

Date: 2026-11-08

Status: accepted; implemented. The preparation stage (Etapa 0) and milestones
M1 to M7 are done; M8 records the qualification. See
[Implementation](#implementation).

## Context

The Agents page shows what a CLI agent did, but only after the fact:

- **Nothing live.** CLI output is held and redacted when the run completes
  (ADR-030), so during a run the UI only sees "Run starting." and "Run in
  progress.". A run that fails midway publishes nothing.
- **Calls without names.** A tool call is recorded as `CLI tool_use received.`,
  without the tool or its target.
- **Reasoning.** Some CLIs may have their reasoning stored as ordinary messages,
  which ADR-012 does not allow.
- **Long runs.** The snapshot loads a run's first 200 events, and a refresh
  clears the live buffer, so the end of a long run is lost.
- **Reading.** The terminal is a flat list of up to 100 small lines.

## Decision

- **One console, two surfaces.** The same console renders on the Agents page
  and in a bottom dock (`Ctrl+J`), hidden on the Agents page. By default the
  dock follows the active agent; the developer can pin one or turn following off.
- **Actions, not text, during a run.** The host publishes action summaries (tool,
  target, status) and a live phase pulse from a closed grammar while the run is
  going. Free text still waits for completion and redaction (an amendment to
  ADR-030 and ADR-012).
- **Failed runs.** A run that fails midway publishes only its action summaries,
  never response text, raw output or result bodies.
- **Reasoning.** No CLI in the corpus prints reasoning text, so the console
  shows the `Thinking` phase and the reasoning token count each CLI reports,
  never text (decided 2026-11-15). Capturing text stays out of scope until a
  CLI provides it.
- **Snapshot from the end.** The snapshot loads a run's last events and merges
  them with the live buffer.
- **Preferences per project.** Dock mode, height, agent and following persist
  with the project.

## Implementation

**M7, the Agents page (2026-11-16).** `agents_view.rs` is split into
`agents_view/` (list, detail, configuration, history, API Mode and free-form
modules). The list groups the pipeline's stages in order, then the support
agents, then the free-form slots with the archived ones folded; each item shows
its status and run time, a transport badge (`CLI · claude`, `API · route`), the
live "now" line and an attention mark. The detail keeps one header over its
tabs (name, stage, status, model, session with copy, and Start, Stop and
Resume) and has Activity, Configuration and History tabs; a History row opens
its run in Activity, and `Ctrl+Shift+[` and `Ctrl+Shift+]` cycle the tabs.

**M5 and M6, the live pulse and reasoning counts (2026-11-16).** While a CLI
runs, the coordinator publishes an ephemeral `terminal://activity` pulse, at
most four a second: the phase (starting, thinking, running a tool, writing,
waiting, finishing), the actions started and finished, with targets from a
closed grammar (a workspace-relative path, a program or a host; anything else
is `withheld`), and counters. Nothing of it is stored. The console shows the
run's actions provisionally until the persisted ones arrive, and the dock, its
strip and the status bar show a "now" line such as
`Running tool · Read src/main.rs · 4 read · 1:12`. Phases the host knows
without a pulse (queued, falling back, retrying, waiting on you) come from the
run and the pulsation. Reasoning shows as episodes and the token count each CLI
reports, never text.

**M3 and M4, the bottom dock (2026-11-16).** The dock sits at the bottom of the
centre column on every view but Agents. `Ctrl+J` hides it and brings it back;
it opens as a strip or expanded, with a sash. Its mode, height, following and
agent persist per project with the UI session, without a migration. By default
it follows the active agent, choosing by pipeline priority with a short
debounce; the developer can pin an agent or turn following off. A status bar
item shows the followed agent and an attention count.

**M2, the console (2026-11-15).** The Agents page shows the selected run as
blocks with their results. The list follows the newest block and pauses when
the developer scrolls up, counting what arrives until `Jump to latest`. The run
header says the status, duration and action count; after the run, consecutive
actions fold into one line (`5 actions · 3 read · 2 searched`). Filters (All,
Actions, Messages, Output, Errors), search, copy and the keyboard work in the
console, and the API Mode viewer uses the same frame.

**M1, action summaries (2026-11-15).** Each adapter reads the tool calls and
results in its frames (`src/terminals/actions.rs`), with tool names from a
closed allowlist per CLI and targets made relative to the run's workspace.
The coordinator records a call as a host line such as `Read · src/main.rs`
and its result as the CLI's text, redacted, or as `Read · ok`, `exit 1`,
`error`, `denied` or `unknown`. A structured `detail_json` column (migration
V67) carries the ordinal, kind, status and duration, never free text. A run
that fails after its stream began publishes only these summaries and an
`Output withheld: …` line with the reason, keeping its event sequence
contiguous. No reasoning frame reaches the audit as text. The held events of
a run are written in one transaction. Snapshots and run selection read a
run's last 500 events, and the Agents page merges a snapshot with live events
instead of replacing them.

**Etapa 0 (2026-11-15).** A corpus of real CLI frames is in
`crates/ikore-terminal/fixtures/cli/<backend>/<version>/`, recorded on Windows
through the pipeline launch path with six fixed, cheap scenarios: Claude
2.1.325, Codex 0.155.1, OpenCode 1.18.32 and Antigravity 1.4.0, plus Gemini
0.60.0's refusal of an account tier. No CLI in it prints reasoning text: Claude
sends empty thinking blocks with token estimates, the others token counts only.
The capture and scrub tools are
ignored tests in `src/terminals/corpus_capture.rs`; a test on every build checks
that the corpus names no person or machine. See the
[corpus notes](../../crates/ikore-terminal/fixtures/cli/CORPUS.md).

The corpus also shows issues in the CLI runtime itself, outside this decision:

- Codex 0.155.1 on Windows, under the read-only sandbox with approvals off,
  rejects every command, reads included, so a Codex stage cannot read the
  workspace there.
- Codex follows the developer's global `AGENTS.md` even with
  `--ignore-user-config` and `--ignore-rules`.
- Gemini 0.60.0 skips the hardening settings file kept in `%TEMP%`, needs a
  trusted folder in headless mode and replaces the `plan` approval mode.

Reasoning blocks already shown during a run that later fails are kept: each
was redacted whole, and the developer asked to see them (decided 2026-11-15).
