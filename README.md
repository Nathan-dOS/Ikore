# Hard Vibe Code

Desktop Orchestrator nativo em GPUI/Rust para Windows x64.

## Estado atual

- `hvc-app` é o único binário desktop e gera `hard-vibe-code.exe`.
- Nine native views are grouped under Agent (Agents, Logs), Engine (Tasks, Feature Forge, Pulsations, Vault), and Companion (Chat, Workspace). Settings opens from the header gear.
- UI e core compartilham o mesmo processo e usam chamadas Rust tipadas, sem JSON/IPC.
- O banco permanece em `%APPDATA%\com.hardvibecode.desktop\hard-vibe-code.db`.
- `HVC_DATA_DIR` permite isolar dados de desenvolvimento e teste.
- Migrations V1–V9 e o serviço `hard-vibe-code.runtime` no keyring foram preservados.

## Stack

- Rust 1.98.0, edition 2024 e Cargo resolver 3.
- GPUI 0.2.2 com backend Windows e patch local auditável.
- Tokio multi-thread para rede, streaming e workers.
- SQLite via `rusqlite`/`refinery`.
- Rope e `EntityInputHandler` para editor Unicode/IME.
- Tree-sitter Markdown incremental e `pulldown-cmark` para preview sem HTML bruto.
- `notify` para filesystem e `keyring` para credenciais.

Não são reutilizadas crates de editor/UI do Zed. A única fonte vendorizada é o
próprio GPUI 0.2.2, sob Apache-2.0, documentado em
[`third_party/gpui/HVC-PATCHES.md`](third_party/gpui/HVC-PATCHES.md).

## Workspace Cargo

- `crates/hvc-app`: composition root e binário GPUI.
- `crates/hvc-core`: domínio, persistência, workspace, LLM, Chat e barramento tipado.
- `crates/hvc-ui`: native shell, three header tabs, nine views, and the GPUI design system.
- `crates/hvc-editor`: Rope, seleção, undo/redo, diff, Markdown e Tree-sitter.
- `crates/hvc-test-support`: ambientes isolados, doubles e fixtures de paridade.

## Executar

```powershell
cargo run -p hvc-app
```

Build release e smoke Windows:

```powershell
cargo build --release -p hvc-app
powershell -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1
```

O artefato atual é o executável release. Instalador NSIS/WiX e Authenticode
pertencem à etapa posterior de distribuição.

## Testes durante o desenvolvimento

Use os atalhos na raiz do workspace. Todos mantêm as dependências do lockfile
(`--locked`) e executam até dois testes simultâneos por binário:

| Comando | Abrangência |
|---|---|
| `cargo test-ui` | UI |
| `cargo test-editor` | Editor e UI consumidora |
| `cargo test-support` | Harnesses, integrações e UI consumidora |
| `cargo test-core` | Core |
| `cargo test-all` | Workspace completo, incluindo integrações, exemplos marcados como teste e doctests |

Os atalhos servem para feedback local. Ao concluir uma alteração, execute
`cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings` e `cargo test-all`.
Mudanças transversais, migrations e dependências exigem o workspace completo.
`cargo test` sem seleção usa apenas o `default-members` (`hvc-app`).

Para investigar uma área menor, confira os nomes com `--list` e aplique um filtro:

```powershell
cargo test --locked -p hvc-core --lib usage:: -- --list
cargo test --locked -p hvc-core --lib usage:: -- --test-threads=2
cargo test --locked -p hvc-test-support --test cli_executor -- --test-threads=2
```

Filtros reduzem a execução, mas ainda compilam o binário de testes da crate.
O primeiro uso de uma seleção pode compilar variantes de dependências com
features diferentes; compare tempos com o mesmo comando e cache aquecido.
Testar um pacote não executa os testes de suas dependências. O nome de um teste
pode diferir do diretório do arquivo (`#[path]`); não derive cobertura só de paths.
Os aliases já contêm `--`: liste com `cargo test-ui --list`, sem outro delimitador.
Para opções do Cargo como `--lib` e `--test`, use o comando explícito acima.
Não passe `--ignored`/`--include-ignored` sem conferir os requisitos de cada caso.

Cada worktree deve usar seu próprio `target/`, sem compartilhar `CARGO_TARGET_DIR`
com outro checkout. `-j` controla jobs de compilação; `--test-threads` controla
testes. A fixture `CoreTestHarness` limita seu runtime Tokio a dois workers;
isso evita multiplicar o número de processadores por teste concorrente.

## Gate da Etapa 7

```powershell
powershell -ExecutionPolicy Bypass -File scripts/gate-stage7.ps1
```

O gate comprova estrutura exclusivamente GPUI/Rust, formatação, Clippy com
`-D warnings`, testes do workspace, build release, smoke Windows e auditorias
Rust de licenças, fontes e advisories.

Reserve esse gate completo para a validação final da entrega. Ele registra
tempo e resultado de cada etapa no console e em `target/stage7/timings.json`,
inclusive a etapa que falhar. Use `-TimingOutputPath` para guardar uma rodada
separadamente. As opções `-SkipAdvisories` e `-EnforceDistribution` permanecem
disponíveis; omitir uma auditoria não constitui o gate final completo.
Compare execuções com cache aquecido e sem outra compilação concorrente;
o primeiro build de uma worktree vazia inclui o custo de compilar dependências.

A [validação desta organização de testes](quality/test-workflow-validation-2026-10-15.md)
registra comandos, tempos, inventário e as pendências encontradas no gate.

## Paridade e desempenho

O ledger [`parity/gpui-frontend-ledger.json`](parity/gpui-frontend-ledger.json)
preserva a rastreabilidade dos 51 testes do frontend removido para seus testes
Rust equivalentes.

No comparativo curto do corte, o P50 até janela visível + SQLite foi de
247,498 ms no GPUI e 528,724 ms no Tauri, redução de 53,19%. A memória ociosa
P50 caiu 61,129%. Os números descrevem a máquina e metodologia registradas no
relatório; não são garantias universais.

A distribuição externa permanece bloqueada até a auditoria final e a
resolução ou decisão jurídica explícita sobre o issue Zed #55470.

## Tab navigation and panel layout

Agents uses a list and one detail panel in CLI and API modes. Pulsations separates Review, Overview, Preparation, and Artifacts; Settings separates Connections, Routes, and Runtime. Agents and Companion share an absolute model-picker overlay with Escape dismissal. Logs has collapsible sections and incremental event display; Vault has independent graph and document scrolling.

Pulsations groups executions by status, with Running first and initially expanded. Each group shows newest-created pulsations first; expansion choices survive view navigation and runtime polling. Review reads complete persisted documents belonging to the current checkpoint, with document/source views, full-content copy, and the existing human decisions. Workspace and Pulsations column dividers support bounded mouse resizing and double-click reset. Widths remain in memory across view navigation. Workspace text, selections, and caret stay clipped to their own viewport; long lines support horizontal scrolling.

The existing session schema persists the active view. The corresponding header tab is derived during restoration; other tab histories and internal sections are kept in memory.

Implementation evidence: [Tabs UI validation](quality/tabs-ui-validation-2026-09-05.md). The accepted design is ADR-015 in the separate Cerebro documentation repository.

Review, status groups, and resizing evidence: [Pulsations and Workspace validation](quality/pulsations-workspace-validation-2026-09-09.md), extending the Review placement defined in ADR-017.


## Feature Forge and task batches

Global **Novo** separates multiple backlog tasks from a new feature conversation.
FeatureAgent refines product scope with the developer; approval creates tasks without
starting execution. Engine provides Tasks, Feature Forge, Pulsations and Vault.

V26 adds developer-started triage/dependency analysis. A validated triage pass may
approve ready scope and create tasks; dependency analysis can apply agent-origin
relations while preserving human ones. V27 parks supported stage failures for
Retry, Revision or confirmed Abort in Pulsations → Review.
See [synchronization and validation](quality/cerebro-sync-validation-2026-09-09.md).

A pulsation selects task revisions and freezes their approved feature context. Pipeline
version 3 keeps all six existing stages and integrates the batch atomically. Database
V24 preserves prior pulsations and adds feature, task, conversation, dependency, wave
and evidence history. Planning notes explicitly describe approved scope, not delivered code.

Implementation: [core services](crates/hvc-core/src/feature_forge/mod.rs).
Validation: [Feature Forge report](quality/feature-forge-validation-2026-09-06.md).

Tasks and Feature Forge refresh from existing events and deliberate actions without a
layout-changing loading row. The Pulsations adapter invalidates them only on newly
observed terminal transitions. Capture and model-picker overlays also close on an
outside click. Companion supports Enter to send, Shift+Enter for a newline, and a
wrapped composer that grows to six visible rows before scrolling internally.
See [UI interaction validation](quality/ui-interactions-validation-2026-09-07.md).
