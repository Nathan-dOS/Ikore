# Hard Vibe Code

Base inicial do MVP do **Desktop Orchestrator**.

## Fase atual

- Marco UI Nativa: Migração para GPUI (Zed Framework)
- Renderização GPU nativa em Rust puro (Metal/Vulkan/DirectX)
- Editor de texto nativo com Tree-sitter para syntax highlighting
- Índice incremental de notas Markdown com frontmatter YAML e wikilinks
- Validação de links quebrados e grafo de contexto na navegação
- Renomeio de nota com atualização automática de referências `[[wikilinks]]`
- Resolução de conflitos de edição externa em abas abertas

## Stack

- GPUI (Zed Framework) — renderização GPU nativa
- Rust 1.88.0 (100% do projeto: UI + core + workers)
- gpui-component (biblioteca community de componentes)
- SQLite via rusqlite + refinery
- cargo test

## Build e execução

1. Build de verificação:
   ```bash
   cargo check
   ```
2. Executar em modo desenvolvimento:
   ```bash
   cargo run
   ```
3. Build de produção:
   ```bash
   cargo build --release
   ```

## Comandos de validação

```bash
cargo check
cargo test
cargo clippy
cargo fmt --check
```

Testes do core:

```bash
cargo test -p hvc-core
```

## Critério de aceite da Fase 1

1. Abrir uma pasta via seletor nativo.
2. Ver a árvore de arquivos `.md` e `.txt` na UI.
3. Alterar um arquivo externamente (ex.: Obsidian) e confirmar evento no painel de watcher.
4. Confirmar que a árvore atualiza automaticamente sem reload.

## Estrutura principal

- `src/`: Binário principal e ponto de entrada GPUI
- `src/views/`: Views GPUI (WorkspaceView, PulsationsView, VaultView, SettingsView, LogsView)
- `src/components/`: Componentes GPUI reutilizáveis (AppSidebar, StatusBar, etc.)
- `src/state/`: Modelos de estado (Model<T> do GPUI)
- `src/core/`: Lógica de negócio, orquestração, agentes
- `src/editor/`: Editor nativo de texto + Tree-sitter
- `src/knowledge/`: Camada de conhecimento (vault, embeddings, índices)
