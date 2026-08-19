# Hard Vibe Code

Base inicial do MVP do **Desktop Orchestrator**.

## Fase atual

- Fase 6: Integração Obsidian + Context Vault UI
- Índice incremental de notas Markdown com frontmatter YAML e wikilinks
- Validação de links quebrados e grafo de contexto na navegação
- Renomeio de nota com atualização automática de referências `[[wikilinks]]`
- Resolução de conflitos de edição externa em abas abertas

## Stack

- Tauri 2
- Rust 1.88.0
- React 18 + TypeScript
- Vite 5
- Vitest 2 + Testing Library

## Rodando com Docker (workflow padrão)

1. Build da imagem:
   ```bash
   docker compose build
   ```
2. Subir frontend (hot reload):
   ```bash
   docker compose up app
   ```
3. Validar backend Tauri/Rust:
   ```bash
   docker compose --profile tools run --rm tauri-check
   ```
4. Build do frontend:
   ```bash
   docker compose --profile tools run --rm frontend-build
   ```
5. Executar testes frontend e Rust:
   ```bash
   docker compose --profile tools run --rm test-runner
   ```

## Ajustando versões

- Copie `.env.example` para `.env` e ajuste:
  - `NODE_IMAGE` (ex: `node:22-bookworm`)
  - `RUST_TOOLCHAIN` (ex: `1.88.0`)

## Execução da janela desktop

Nesta etapa, o Docker controla versões e validações de build.
Para abrir a janela nativa do Tauri localmente, rode no host:

```bash
npm install
npm run tauri:dev
```

## Comandos de validação

```bash
npm run check:ts
npm run check:tauri
npm run test
npm run test:rust
npm run check:all
```

Cobertura frontend:

```bash
npm run test:coverage
```

## Critério de aceite da Fase 1

1. Abrir uma pasta via seletor nativo.
2. Ver a árvore de arquivos `.md` e `.txt` na UI.
3. Alterar um arquivo externamente (ex.: Obsidian) e confirmar evento no painel de watcher.
4. Confirmar que a árvore atualiza automaticamente sem reload.

## Estrutura principal

- `src/`: UI React
- `src/ipc/`: contratos e client IPC tipados
- `src/test/`: setup e mocks compartilhados do Vitest
- `src-tauri/`: core Rust/Tauri
- `src-tauri/tests/`: testes de integração e fixtures Rust
- `docker-compose.yml`: serviços de desenvolvimento
- `Dockerfile`: ambiente base do projeto
