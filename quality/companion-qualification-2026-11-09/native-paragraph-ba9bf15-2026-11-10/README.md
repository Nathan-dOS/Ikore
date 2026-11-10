# Native paragraph visual QA fixture — 2026-11-09

Prepared artifact only. No database has been seeded by preparing these files.
This script is outside application sources and is not a migration or executable.
It uses Python 3.10+ and the standard library, without Cargo or package installation.

## Future use after the regression gate

1. Keep the GUI closed. Choose a **new, nonexistent** root below this checkout's
   `HARD-VC/target/`, separate from this artifact directory. Start the existing
   `pulsation_ui_fixture` example with that root using the root agent's selected
   build command. Do not use `--resume` for the first run. Wait for the example's
   `Loopback provider ready` output and `M5_UI_FIXTURE` marker.
2. Keep the example running. In a second PowerShell terminal, set the explicit
   newly initialized root and seed once:

   ```powershell
   Set-Location '<repo>'
   $qaRoot = '<repo>\target\native-paragraph-qa-2026-11-09'
   python .\target\companion-final\native-paragraph-fixture-2026-11-09\seed_native_paragraph_fixture.py seed $qaRoot
   ```

   The path is an example, not an existing profile and not a default. If that
   directory already exists before the initializer runs, choose a new name.
   The script refuses a used profile, including any existing QA IDs. It never
   clears or overwrites existing chat/dock/draft rows.
3. Set `HVC_DATA_DIR` to `Join-Path $qaRoot 'data'` only in the dedicated app
   process environment. The root agent owns launching and controlling the app
   with Sky. The normal application profile must never be used for this QA.
4. M5 Segundo remains the initializer's last active project. Full Chat restores
   the third conversation; the Companion is open with three chat topics. The
   first conversation has a separate unsent Unicode draft. Native behavior may
   show a link for the full Chat conversation to avoid mounting its composer
   twice. Responsive width determines how many topics fit; three are persisted.
5. Close the GUI after QA, then stop the fixture with its documented STOP file:

   ```powershell
   New-Item -ItemType File -Path (Join-Path $qaRoot 'STOP')
   ```

   The example also expires after 45 minutes. The seeder does not start, probe,
   extend or stop the server. The root agent controls its lifecycle.

## Read-only evidence

Take this snapshot before and after the native interactions. It opens SQLite
with `mode=ro` and `query_only=ON`; it prints only counts and draft revision/text:
JSON stdout uses ASCII escapes so Windows cp1252 terminals can print every
result safely. JSON decoding restores the exact Unicode draft; the stored
draft/message content is unchanged.

```powershell
python .\target\companion-final\native-paragraph-fixture-2026-11-09\seed_native_paragraph_fixture.py snapshot $qaRoot
```

Immediately after seeding, expect 3 QA conversations, 6 completed QA messages,
0 noncompleted QA messages, 0 QA model calls, 0 QA tool calls, 1 open dock,
3 dock topics, 1 active QA Chat and 1 draft at revision 1. The `pulsations`,
`jobs` and `checkpoints` counts expose only existing initializer state; opening
the GUI can let its real supervisor advance queued initializer work. The script
does not alter or freeze that work. After interactions, layout/draft counts or
revisions may legitimately change; compare the recorded draft text as required.

The draft starts `Rascunho Unicode não enviado — ação, coração, café; Ω, 漢字, 🙂.`
and contains two further lines, including a combining accent. It is never
inserted into `chat_messages`.

## Scope and guards

- Anchor: this exact script placement under this checkout's `target/`; resolved
  profile, database, marker and SQLite sidecars must stay in the permitted tree.
  A redirected target/data directory or database is refused, as are linked
  database/sidecar files. There is no default root or
  arbitrary database argument. SQLite `mode=rw` refuses database creation.
- Required marker bytes: `pulsation-ui-fixture-v1\n`. Required projects:
  exactly M5 Principal and M5 Segundo, unarchived, with their workspaces inside
  the supplied isolated root. M5 Segundo must be last active by the core's query.
- Every credential reference must equal `m5-ui-fixture:` plus its connection ID.
  Exactly one connection must be enabled: `legacy-openai`, provider `ollama`,
  active, with an explicit HTTP loopback IP and port. Routing v2 must remain off.
  Disabled connections can retain the initializer's provider defaults; they are
  not enabled or changed. The OS credential store is never accessed.
- Read-only validation precedes `BEGIN IMMEDIATE` and a repeated validation.
  SQLite's authorizer allows inserts only into chat conversations/messages,
  Companion layout and drafts; only `active_view`,
  `active_chat_conversation_id`, `updated_at` may be updated in `ui_sessions`.
  All other writes, deletes, DDL, attached databases and trigger writes fail.
  The read-only `PRAGMA main.data_version` counter is permitted only with no
  argument: the guarded fresh profile's read-only fingerprint produced exactly
  authorizer opcode 19 with `data_version`, null argument, `main`, no trigger.
  Observed tables: `companion_agent_output_fts` and
  `companion_pulsation_digest_fts`. This is needed by SQLite FTS reads;
  assignment forms and other databases
  remain denied. The initial denied seed rolled back with all QA counts zero.
  A transaction fingerprint checks all protected tables/remaining UI fields
  before commit; failure rolls the whole seed back. Existing UI tabs and the
  active pulsation reference remain intact. No routes, provider configuration,
  credentials, pipeline, jobs, checkpoints or approvals are changed.
- Synthetic data: three titles and one user/assistant pair per conversation,
  six completed messages total. Multi-paragraph Markdown includes accents,
  emoji, CJK, a table, Rust code and a diff. Conversation snapshots copy the
  already isolated loopback connection's default model and use API/manual
  metadata. They are display fixtures, not evidence that a model ran.
- No send command, network call, model call, tool call, approval or pipeline
  transition is performed by this script. The six stages and four human
  checkpoints remain governed by the application. GUI execution is a separate
  QA action; its supervisor may process the initializer's existing queued work.
- Keep the GUI closed during seeding. The script cannot prove the server is
  alive or the GUI is closed and cannot defend against another process
  maliciously replacing directories during the short validation/write window.
  It assumes a trusted local QA operator and an unmodified initializer.
- This is persistence preparation, not visual qualification. It cannot certify
  real font geometry, selection behavior, DPI rendering, performance or live
  provider behavior. Those require the root agent's native QA and gate record.

Schema/contract references inspected: `docs/architecture.md`,
`crates/hvc-core/migrations/V9__chat_companion.sql`,
`V46__antigravity_cli_backend.sql`, `V65__companion_dock.sql`,
`crates/hvc-core/src/chat_service.rs` mappers,
`companion_dock.rs`, `persistence_service.rs::save_ui_session` and its active
project query, plus `pulsation_ui_fixture.rs` and `configure_loopback`.

## Non-database checks

```powershell
python .\target\companion-final\native-paragraph-fixture-2026-11-09\seed_native_paragraph_fixture.py self-check
```

`self-check` checks synthetic message/draft boundaries and refusal rules with
pure assertions; it opens no database and reads/writes no profile. Only these
artifact checks are appropriate before the gate. A real seed and native QA have
not been executed as part of this artifact preparation.

It also checks that the real CLI JSON formatter round-trips the Unicode draft
through cp1252 and ASCII, preventing a terminal encoding error after commit.
The authorizer checks cover read-only data_version acceptance and refusal of
its argument/other-database forms, journal-mode changes and protected writes.
