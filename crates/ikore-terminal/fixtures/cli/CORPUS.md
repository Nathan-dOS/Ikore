# Real CLI frame corpus (ADR-040, Etapa 0)

The `current/` and `minimum/` folders hold hand-written frames. The folders named
after a version (`<backend>/<x.y.z>/`) hold real frames: what each CLI printed
on 2026-11-15, on Windows 11, launched the way a pipeline stage is launched
(adapter arguments, `Hardening::Pipeline`, trusted executable resolution and a
cleared environment). `src/terminals/corpus_capture.rs` records and scrubs them;
its module comment has the commands.

## Files

- `<scenario>.jsonl`: the stdout lines, one frame per line, as printed.
- `<scenario>.stderr.txt`: the stderr lines, when there were any.

Every scenario ran in a fresh workspace with `notes.txt`, `src/lib.rs` and
`config.env`. The token in `config.env` is a synthetic canary
(`synthetic-corpus-canary-7c1e`), never a credential.

| Scenario | Prompt, in short | Effort |
|---|---|---|
| `tool-ok` | read `notes.txt`, reply with its second line | default |
| `tool-error` | read `missing-notes.txt`, reply MISSING if it fails | default |
| `denied` | create `created.txt`, reply DENIED if it cannot | default |
| `reasoning` | the bat-and-ball question, number only | high, where the adapter takes one |
| `secret-read` | count the lines of `config.env` | default |
| `cancel` | summarize every file; the capture kills the process tree at the first tool call | default |

## Versions and models

| Backend | Version | Model | Scenarios |
|---|---|---|---|
| Codex | 0.155.1 | CLI default | all six |
| OpenCode | 1.18.32 | `groq/openai/gpt-oss-20b` | all six |
| Antigravity | 1.4.0 | `gemini-3.8-flash` | all six |
| Gemini | 0.60.0 | `gemini-2.5-flash` | `account-ineligible` only |

Codex 0.155.1, Gemini 0.60.0 and Antigravity 1.4.0 sit just above the validated
ranges in `adapters/mod.rs`; their corpus is what widening a range needs.

## Not recorded yet

- **Claude.** The CLI on the capturing machine was signed out (expired OAuth
  session). Capture it after `claude auth login`.
- **Gemini.** The account's tier ("Gemini Code Assist for individuals") is no
  longer accepted by the CLI, which points to Antigravity. Only that failure is
  recorded.
- **Rate limit and truncation.** Not reproducible with a cheap, fixed prompt.
  The quota fixtures (`fixtures/quota/`) and the hand-written frames still cover
  their shapes.
- **Minimum versions.** Recording them means installing older CLIs.

## What these frames show

- **Codex 0.155.1, Windows, `--sandbox read-only -a never`.** Every command the
  model tried, including plain reads, was rejected ("blocked by policy") before
  an item existed. Only stderr says so; stdout has agent messages and no
  `command_execution` item. The model also followed the developer's global
  `~/.codex/AGENTS.md`, which `--ignore-user-config` and `--ignore-rules` do not
  exclude.
- **Gemini 0.60.0.** Besides the tier: the hardening settings file in `%TEMP%`
  is skipped as insecure, headless runs need a trusted folder (`--skip-trust` or
  `GEMINI_CLI_TRUST_WORKSPACE=true`), and `--approval-mode plan` falls back to
  `default` in an untrusted folder.
- **OpenCode 1.18.32.** One `tool_use` frame carries the call and its outcome
  (`part.state.status`), paired by `part.callID`. A read tool's output carries
  the file's content.
- **Antigravity 1.4.0.** A tool step goes `ACTIVE`, then `DONE` or `ERROR`, under
  one `step_index`. File reads report a size ("2 lines, 79 bytes"), not content.
  Thinking shows only as `thinking_tokens`.
- **Reasoning text.** No backend in this corpus printed reasoning text: Codex
  emitted no `reasoning` item, OpenCode no `reasoning` part, Antigravity only a
  token count.

## Scrubbing

The scrub replaces the workspace and home paths at any escaping depth, the user
and machine names, Windows SIDs, e-mail addresses, UUIDs and provider ids (with
stable stand-ins, so a call and its result still pair), and any extra private
words passed at scrub time. A model can repeat a path in pieces across text
deltas, so the workspace was a neutral folder (`C:\Users\Public\ikore-corpus`).
Every file was read by hand before it was committed, and
`the_committed_corpus_names_no_one` guards the folders from now on.
