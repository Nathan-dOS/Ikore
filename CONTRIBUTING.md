# Contributing to Ikore

Thank you for your interest in contributing to **Ikore**.

Ikore is a native desktop orchestrator written in Rust with GPUI. It guides AI
agents through an engineering pipeline with human checkpoints. Each run (a
*pulsation*) goes through six stages: **Intake → Specification → Contract →
Code → Test → Integrator**. Stages run through LLM APIs or official CLIs:
Claude Code, Codex, Gemini, OpenCode, Antigravity and custom adapters. Project
context comes from the workspace Vault (Markdown notes, Tree-sitter and local
embeddings through Ollama). All state lives in a local SQLite database, and
credentials live in the operating system keyring.

See [docs/architecture.md](docs/architecture.md) for the architecture and
[docs/testing.md](docs/testing.md) for the full testing guide. For a short,
visual overview of the path from an idea to a merged pull request, read
[How to contribute, step by step](docs/how-to-contribute.md). Everyone who
participates follows the [Code of Conduct](CODE_OF_CONDUCT.md).

We welcome bug fixes, tests, documentation, performance work, new adapters,
and Linux/macOS portability of the headless crates.

---

## 1. Principles

- **Respectful technical review:** critique the code and the design, never the
  person.
- **Focused scope:** prefer small, readable and testable changes with clear
  responsibilities.
- **Transparency:** describe in the pull request any change to a contract,
  schema, migration, environment variable or visible behavior.
- **Security:** never report vulnerabilities in a public issue. Report them
  privately as the [security policy](SECURITY.md) describes.
- **English everywhere:** code, comments, identifiers, UI text, commit
  messages, issues, pull requests and documentation are written in English.

---

## 2. Supported platforms

| Platform | What works |
|---|---|
| **Windows 10/11 x64** | Everything: the desktop app (`ikore-app`), the UI (`ikore-ui`), the complete test suite, release builds and the smoke test. This is the CI reference platform. |
| **Linux / macOS** | Only the headless crates (`ikore-contracts`, `ikore-terminal`, `ikore-voice`, `ikore-core`, `ikore-editor`, `ikore-test-support`), natively with `-p` or through Docker. Voice dictation reports itself unsupported there. GPUI is configured with the Windows backend only, so `ikore-ui` and `ikore-app` do not build on these platforms. |

Some tests still fail on Linux because of platform-specific code (CLI process
and terminal handling). They are tracked in dedicated issues. If you work on
Linux or macOS, they are a great place to start.

---

## 3. How to contribute

### 3.1. Reporting bugs

Search for an existing issue first. When you open a new one, use the bug
template and include:

1. the expected behavior and the current behavior;
2. steps to reproduce, with logs or a stack trace (**remove** tokens, keys,
   personal paths and private prompt content);
3. your environment: Windows version, `rustc --version`, the Ikore commit, the
   CLI backend or provider involved, and whether you used Docker.

### 3.2. Proposing features or architectural changes

Clear bug fixes, tests, documentation and local refactors can go straight to a
pull request.

**Open an issue first** for large or architectural changes, for example:

- a new pipeline stage or a change to the contract between stages;
- a new migration or a SQLite schema change;
- a new CLI backend or LLM provider;
- a heavy new dependency in `Cargo.toml`, or a change to the vendored GPUI
  (`third_party/gpui`);
- a change to the human checkpoint flow (Review, Confirm/Flag,
  Retry/Revision/Abort).

That way the design is agreed before you invest time in the implementation.

---

## 4. Workflow (fork and pull request)

### 4.1. Fork, clone and branch

```bash
git clone https://github.com/<your-user>/Ikore.git
cd Ikore
git remote add upstream https://github.com/Nathan-dOS/Ikore.git
```

Create a descriptive branch from an up-to-date `main`:

- `feat/<description>`: new feature
- `fix/<description>`: bug fix
- `docs/<description>`: documentation
- `refactor/<module>`: refactor without external behavior changes
- `test/<area>`: tests
- `ci/<description>`: GitHub Actions, Docker and gate scripts

### 4.2. Local environment

**Windows (recommended):**

1. Install [rustup](https://rustup.rs). `rust-toolchain.toml` pins Rust
   **1.98.0** with `clippy` and `rustfmt`, and rustup installs it on the first
   `cargo` call.
2. Install **Visual Studio Build Tools 2022** with the "Desktop development
   with C++" workload and a recent Windows SDK (MSVC toolchain).
3. Run the app with isolated data so it never touches your real profile:

   ```powershell
   $env:IKORE_DATA_DIR = "$PWD\target\dev-data"
   cargo run -p ikore-app
   ```

**Linux/macOS (headless crates):**

```bash
cargo test --locked -p ikore-core -p ikore-editor -p ikore-test-support -- --test-threads=2
```

**Docker (headless Linux, same environment as CI):**

```bash
docker compose --profile tools build
docker compose --profile tools run --rm test-runner
docker compose --profile tools run --rm clippy-check
docker compose --profile tools run --rm fmt-check
```

`.env.example` **only** holds the Docker image versions (`RUST_IMAGE`,
`RUST_TOOLCHAIN`). Ikore does **not** read API keys from `.env` files: provider
credentials are entered in Settings and stored in the operating system
keyring.

### 4.3. Checks before committing (required)

On its own, GitHub Actions only checks formatting and audits dependencies. The
complete workspace suite on Windows, the integration tests and the headless
Linux harness in Docker run only when a maintainer dispatches them by hand.
These local checks are the real gate for your pull request:

```powershell
# 1. Formatting
cargo fmt --all -- --check

# 2. Strict lint
cargo clippy --workspace --all-targets --locked -- -D warnings

# 3. Tests for the area you changed
cargo test-core      # ikore-core
cargo test-ui        # ikore-ui
cargo test-editor    # ikore-editor + consuming UI
cargo test-support   # harnesses/integration tests + consuming UI
```

Also run `cargo test-all` (the complete workspace, about 7 minutes on a
12-core machine) for cross-cutting changes: migrations, dependencies, stage
contracts or the CLI runtime.

To investigate a subset, check the test names with `--list` before filtering.
A filter that selects zero tests proves nothing:

```powershell
cargo test --locked -p ikore-core --lib usage:: -- --list
cargo test --locked -p ikore-test-support --test cli_executor -- --test-threads=2
```

### 4.4. Commit messages (Conventional Commits)

Write commit messages in English, with a type and a scope:

```text
feat(core): add revision diff to the specification brief
fix(terminals): classify synthetic Claude API errors as failures
feat(ui): add selectable chat messages
test(brownfield): cover incremental mapping of renamed files
ci: run the integration suite on windows-latest
docs: document the Docker headless harness
```

Scopes already used in the history: `core`, `ui`, `brownfield`, `terminals`,
`orchestration`, `forge`, `test-support`, `composition`, `embeddings`, `gpui`,
`docker`, `app`. Combine scopes when needed (`feat(core,ui): ...`).

---

## 5. Architecture and quality guidelines

1. **Crate boundaries**
   - `ikore-core` is framework-agnostic: domain, migrations, workspace, LLM
     routing, orchestration and terminal host integration. It must **not** depend on `gpui`.
   - `ikore-contracts` owns shared values. `ikore-terminal` owns CLI services,
     repository, process isolation and quota. Neither depends on core, UI or
     test-support, including development dependencies. Run
     `scripts/check-crate-boundaries.ps1` to validate the graph.
   - `ikore-voice` owns the speech runtime install, microphone capture, global
     push-to-talk and earcons. It is a leaf: no local crate and no `gpui`,
     including development dependencies. Its Win32 calls stay behind
     `#[cfg(windows)]` with a `SAFETY` comment on every `unsafe` block.
   - `ikore-ui` calls the core through typed Rust APIs, with no JSON and no IPC.
   - `ikore-editor` owns the rope, selection, undo/redo, diff, Markdown and
     Tree-sitter support.
   - `ikore-test-support` holds harnesses, doubles and fixtures. Never use it in
     production code.

2. **Error handling**
   - Do not use `.unwrap()`/`.expect()` on production paths that involve I/O,
     networking, processes, JSON/YAML parsing, or LLM and CLI output.
     Propagate a `Result` with context (`thiserror` in the core, `anyhow` at
     application boundaries).
   - `expect` is acceptable only for internal invariants that cannot be
     violated, with a message that explains the invariant.

3. **Persistence and migrations**
   - Never edit an existing migration in `crates/ikore-core/migrations/`:
     `refinery` validates their checksums. Add the next version
     (`V<N+1>__*.sql`).
   - Every new migration updates the `MAX(version)` assertions in
     `crates/ikore-test-support/tests/serial_upgrade.rs` and adds a
     `vN_to_vN+1` test.

4. **Agent artifacts and language**
   - Agent-generated artifacts are validated as **English** (English-only
     policy), and the interface text is in English.
   - New code uses English identifiers, comments and messages. Some older
     comments and script messages are still in Portuguese. Translating them is
     a welcome contribution. Test fixtures that are intentionally non-English
     (for example, the language-guard tests) must stay as they are.

5. **Vendored GPUI**
   - Changes to `third_party/gpui` require a new entry in
     [`third_party/gpui/HVC-PATCHES.md`](third_party/gpui/HVC-PATCHES.md)
     (`HVC-GPUI-00N`) with the file, reason, origin and validation. The GPUI
     Apache-2.0 license must be preserved.

6. **Security and secrets**
   - **Never** include API keys, tokens, credentials, personal paths or private
     data in code, tests, fixtures, logs or examples.
   - Use obviously synthetic values (`fixture-secret-value`, `example.test`).
     The core's `secret_redactor` protects logs. Do not bypass it.

7. **Tests**
   - Features and fixes come with unit or integration tests that prevent
     regressions.
   - Harness rules: isolated disk (temporary directories), isolated keyring,
     **no external network**, ephemeral SQLite migrated from scratch,
     order-independent tests and automatic cleanup.
   - Tests that need an authenticated CLI, network access or paid quota use
     `#[ignore = "Requires <reason>"]` and never run in CI.
   - Do not use retries, long sleeps or `#[ignore]` to hide flakiness. Open an
     issue and fix the cause.
   - Time-sensitive tests must tolerate CI runners that are slower than your
     machine. Prefer synchronizing on events over short fixed deadlines.

8. **Rust style**
   - The seven application packages inherit selected workspace lints: unwrap
     usage, wildcard imports, documentation Markdown and undocumented unsafe
     blocks. CI retains `-D warnings`. `clippy.toml` permits unwraps in test
     functions/modules; test-only harness allowances have an explicit reason.
     Production I/O failures still require a `Result`, not a replacement
     `expect`.
   - Use the existing formatter defaults. Do not introduce a global formatting
     configuration or reformat vendored GPUI as part of an application
     cleanup. Keep exhaustive internal event matches and justify each public
     API change separately.

---

## 6. AI-assisted contributions

Ikore itself orchestrates AI agents, so we **encourage** the use of Claude Code,
Codex, Cursor, Copilot and similar tools, as long as you follow these
human-in-the-loop rules:

1. **You are the author and you answer for every line of your pull request.**
   Do not submit generated code that you have not read, understood and tested.
2. **Small, reviewable pull requests.** Split large deliveries into cohesive
   pull requests. A pull request with thousands of generated lines will be sent
   back.
3. **No generation noise.** Remove comments that only narrate the obvious,
   duplicated functions, unused dependencies, temporary files and intermediate
   agent reports.
4. **Say in the pull request that you used AI** when relevant parts were
   generated. It helps calibrate the review and is never a reason to reject the
   contribution.

---

## 7. Pull request checklist

The pull request template includes these items:

- [ ] **What changed and why**, referencing the issue (`Closes #12`).
- [ ] **How to test** it locally, with commands and steps.
- [ ] `cargo fmt --all -- --check` and
      `cargo clippy --workspace --all-targets --locked -- -D warnings` pass.
- [ ] Tests for the changed area pass locally. The full CI is green.
- [ ] New migrations, contracts or dependencies are justified and covered by
      tests.
- [ ] If you changed `Dockerfile`, `docker-compose.yml`, `.env.example` or the
      workflows: the Docker build still works and `README.md` is updated.
- [ ] If you changed `third_party/gpui`: there is a new entry in
      `HVC-PATCHES.md`.
- [ ] No secret, personal path or private data was included.

---

## 8. License

Ikore is free software, licensed under the
[GNU General Public License v3.0 or later](LICENSE) (`GPL-3.0-or-later`). By
submitting a contribution, you agree that it is licensed under the same terms.
The vendored code in `third_party/gpui` keeps its own Apache-2.0 license and
notices, and the Lucide icons in `crates/ikore-ui/assets/icons` keep their ISC
license.

Source code is published; binary releases are not available yet. See
`quality/distribution-policy.json` for the current distribution policy.
