# Hard Vibe Code

Desktop Orchestrator em migração lado a lado de Tauri/React para GPUI/Rust.

## Estado atual

- A branch de migração é `codex/gpui-migration`.
- O worktree anterior à migração foi preservado em um checkpoint Git.
- O binário novo é `hard-vibe-code`, implementado pelo crate `hvc-app`.
- Tauri/React continua funcional como adaptador legado até a paridade e aprovação dos gates.
- Windows x64 é a plataforma do primeiro corte.
- Chat Companion é uma das seis views obrigatórias.

## Stack nativa

- Rust 1.98.0, edition 2024 e Cargo resolver 3.
- GPUI 0.2.2 com backend Windows integrado.
- Tokio multi-thread para rede, streaming e workers.
- SQLite via `rusqlite`/`refinery`, migrations V1–V9.
- Rope + `EntityInputHandler` para editor Unicode/IME.
- Tree-sitter Markdown incremental e `pulldown-cmark` para preview sem HTML bruto.
- `notify` para filesystem e `keyring` para credenciais.

Não são reutilizadas crates de editor/UI do Zed sujeitas a GPL. A distribuição externa permanece bloqueada até a auditoria de licenças e a resolução/decisão jurídica sobre dependências transitivas apontadas no issue Zed #55470.

## Workspace Cargo

- `crates/hvc-app`: composition root e binário GPUI.
- `crates/hvc-core`: domínio, persistência, workspace, LLM, Chat e barramento tipado.
- `crates/hvc-ui`: shell, design system, seis views e editor conectado ao GPUI.
- `crates/hvc-editor`: Rope, seleção, undo/redo, diff, Markdown e Tree-sitter.
- `src-tauri`: adaptador legado temporário, consumidor de `hvc-core`.
- `src`: frontend React legado temporário.

O banco continua em `%APPDATA%\com.hardvibecode.desktop\hard-vibe-code.db`. `HVC_DATA_DIR` permite isolar dados de desenvolvimento/teste. Um lock exclusivo impede Tauri e GPUI de operar simultaneamente no mesmo diretório.

## Executar o aplicativo GPUI

```powershell
cargo run -p hvc-app
```

Build release do primeiro artefato Windows:

```powershell
cargo build --release -p hvc-app
powershell -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1
```

Esse comando gera um executável; instalador NSIS/WiX e Authenticode pertencem à etapa posterior de distribuição.

## Gates Rust

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p hvc-app
```

## Baseline Tauri/React durante a migração

```powershell
npm.cmd test
npm.cmd run build
cargo check -p hvc-tauri-legacy
```

O frontend, `package.json`, Vite, Monaco, Zustand, IPC e Tauri só serão removidos quando houver equivalência dos testes frontend, paridade funcional das seis views, smoke test Windows e aprovação dos benchmarks definidos na ADR-006.

## Limitações atuais da migração

- O shell GPUI, as seis rotas, o editor Rope/IME, a restauração V8/V9 e a ressincronização após atraso do barramento estão implementados.
- A paridade funcional de Workspace ainda precisa ligar árvore, múltiplas abas, preview, autosave persistido e o fluxo completo de diff/HITL à UI nativa.
- Pulsations, Vault, Settings, Logs e Chat já têm entidades e painéis próprios, mas suas operações completas, streaming/cancelamento e testes de fluxo ainda não substituem as views React.
- Benchmarks comparativos, equivalência Rust dos 51 testes frontend, auditoria de licenças/advisories e decisão sobre a dependência GPL continuam pendentes.

Última validação local desta etapa: 107 testes Rust do workspace e 51 testes frontend aprovados, além de Clippy com `-D warnings`, builds frontend/release e smoke Windows com shutdown gracioso.
