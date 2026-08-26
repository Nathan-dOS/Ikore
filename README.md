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
- `crates/hvc-test-support`: ambientes isolados, doubles determinísticos e fixtures HTTP/SSE para os testes de paridade.
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

Gate agregado da Etapa 6:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/gate-stage6.ps1
```

O relatório, metodologia e evidências estão em [`quality/stage6-report.md`](quality/stage6-report.md). O gate integral está atualmente reprovado pelos thresholds de cold start/frame cadence; não use a mensagem do gate técnico sem benchmarks como autorização de corte.

## Baseline Tauri/React durante a migração

```powershell
npm.cmd test
npm.cmd run build
cargo check -p hvc-tauri-legacy
```

O frontend, `package.json`, Vite, Monaco, Zustand, IPC e Tauri só serão removidos quando houver equivalência dos testes frontend, paridade funcional das seis views, smoke test Windows e aprovação dos benchmarks definidos na ADR-006.

## Controle de paridade

O inventário versionado [`parity/gpui-frontend-ledger.json`](parity/gpui-frontend-ledger.json) mapeia nominalmente cada um dos 51 testes React para seu alvo Rust. A validação estrutural do ledger faz parte de `cargo test --workspace` e impede casos ausentes, duplicados ou sem justificativa.

Estado ao concluir a Etapa 5: os 51 casos do baseline React estão cobertos por testes Rust equivalentes. O ledger não possui casos parciais ou pendentes; isso comprova paridade funcional por cenário, mas a remoção do legado continua bloqueada pelos gates das Etapas 6 e 7.

## Limitações atuais da migração

- O shell GPUI, as seis rotas, sidebar recolhível, badges semânticos, fila de avisos, polling serial, persistência com debounce/flush, restauração completa V8/V9 e ressincronização após atraso do barramento estão implementados.
- Workspace possui árvore nativa, abas independentes, editor Rope virtualizado, cursor/scroll restauráveis, preview Markdown seguro, Tree-sitter incremental para `.md`, `.txt` plain text, autosave atômico de 1 segundo por arquivo, conflitos explícitos e diff/HITL com Enter/R.
- Pulsations possui criação, seleção, restauração, cancelamento de jobs, pipeline nativo de seis estágios, contadores e artefatos navegáveis sem bloquear a thread GPUI.
- Vault possui árvore, índice, busca substring/regex, grafo determinístico clicável, links quebrados, frontmatter, preview seguro, renomeação coordenada, cache de documentos e refresh do watcher com debounce.
- Settings possui profiles, conexões/keyring, rotas e targets ordenados, bindings exatos, modos off/shadow/on, configuração YAML Graph/RAG, embeddings, teste assíncrono e reset de resiliência. Segredos são mascarados, redigidos de `Debug` e limpos antes do primeiro `await`.
- Logs inspeciona `agent_runs`, payloads raw/estruturados, erros, tokens/custo, métricas de roteamento, saída visível e feeds watcher/LLM limitados a 200 registros. Duplicatas são descartadas e gaps recarregam o snapshot persistido sem aceitar resultados obsoletos.
- Chat possui entidade, backend tipado e painel GPUI próprios, conversas por projeto, seleção restaurável, filtro de conexões elegíveis, disclosure, composer, streaming incremental, cancelamento, ferramentas read-only, Markdown seguro, diff somente para cópia e recovery por snapshot.
- O hardening e as auditorias da Etapa 6 estão implementados. Licenças/fontes passaram tecnicamente, RustSec/npm ficaram em zero vulnerabilidades e memória melhorou 60,831%; cold start melhorou apenas 14,566% e frame cadence ficou em P95 18,2096 ms/P99 19,4284 ms num display de 60 Hz. Esses dois gates e o bloqueio jurídico do issue #55470 impedem o corte.

Validação da Etapa 6: 186 testes Rust (185 unitários e 1 integração), 51 testes frontend, formatação, Clippy com `-D warnings`, auditorias sem vulnerabilidades, builds release dos dois shells e smoke GPUI aprovados. Os resultados de desempenho permanecem formalmente reprovados conforme o relatório versionado.
