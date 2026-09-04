# Checkpoint BF1 — Onboarding Brownfield

Data: 2026-08-29. Branch `gpui-migration`. Baseline: `1ae88a7` (M5 funcional e smoke visual concluídos). Estado: **BF1-G aprovado; BF2 não iniciada**.

## Resultado

BF1 foi corrigida e concluída como uma anexação não destrutiva. Abrir um workspace existente apenas o classifica como brownfield e apresenta o consentimento; não executa inventário, não cria o vault e não altera o bootstrap greenfield. O primeiro inventário é uma ação explícita da UI, executada fora da thread de renderização, e não cria job persistente. Jobs de manutenção persistentes continuam reservados para BF2+.

O rollout de preparação do G5 continua independente e desligado (`preparation_enabled=false`). Nenhum contrato ou arquivo da sincronização M5 foi alterado nesta fase.

## Evidências do escopo BF1

| Item | Evidência |
|---|---|
| BF1.1 — detecção e escolha | `WorkspaceKind` separa greenfield/brownfield. `AppCore::open_workspace` mantém o bootstrap apenas no ramo greenfield. A UI oferece “Iniciar mapeamento gradual” e “Usar como projeto novo (sem mapeamento)”. |
| BF1.2 — consentimento | `runtime_settings` usa a chave isolada `brownfield.onboarding_consent.<project_id>`. Ausência, `false` e `true` representam não decidido, recusado e concedido. Leitura/upsert validam projeto ativo e valor; UI permite recusar, reativar e revogar. |
| BF1.3 — inventário limitado | `InventoryScanner` enumera paths de manifests, linguagens e pontos de entrada usando `CollectionBudget`, `CancellationToken`, `ContextCoverage`, `.gitignore` e filtros de segurança. `read_bytes=0`: nenhum conteúdo é aberto pelo inventário e nenhum script/build é executado. |
| BF1.4 — primeira tarefa | `LexicalCodeContextService` permaneceu independente de vault/inventário/AST. Regressões existentes `first_request_without_vault_uses_exact_bytes_and_unresolved_references` e `existing_safe_reader_handles_code_without_vault_and_sources_stay_project_scoped` passaram. |
| BF1.5 — estrutura mínima | Somente após consentimento são criados `knowledge/`, `knowledge/obsidian/` e `knowledge/obsidian/components/`. `components/` permanece vazio; nenhum template, base, nota ou `docs/PROJECT_AGENT.md` é criado. Componentes junction/symlink são rejeitados. |
| BF1.6 — matriz | Rust+TypeScript, ausência de manifests, `.gitignore` com padrão aninhado/negação, orçamento/cancelamento, isolamento entre projetos/raízes, revogação, zero jobs sem consentimento, segredos/binários/`.env*` e junctions cobertos por testes. |

## Invariantes e limites

- Conteúdo humano é preservado byte a byte nos testes de adoção.
- Consentimento de outro projeto é rejeitado pelo fence workspace/projeto.
- Sem consentimento há zero inventário, zero criação de diretório e zero registros em `jobs`.
- Nenhuma chamada LLM ou embedding existe no caminho BF1.
- Migrations permanecem V1–V13; BF1 não adiciona nem edita migration.
- Não foram adicionados Tree-sitter, resolvedores, cache/arestas, notas de componente, frontier ou vetores.
- Respostas assíncronas da UI são cercadas por projeto e geração do onboarding.

## Validação

| Gate | Resultado |
|---|---|
| `cargo fmt --all -- --check` | aprovado |
| `cargo test --locked -p hvc-core` | 204 aprovados |
| `cargo test --locked -p hvc-ui` | 65 aprovados |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | aprovado |
| `cargo test --workspace --locked` | 327 aprovados, 0 falhas na repetição integral |
| `cargo build --release --locked -p hvc-app` | aprovado |
| `git diff --check` | aprovado |

Na primeira execução integral, `real_inflight_learning_yields_to_foreground_or_kill_switch_without_late_publication` falhou por timing antes de qualquer alteração naquele subsistema. O teste passou isoladamente e a repetição integral terminou com 327/327; nenhuma correção M5 foi necessária.

## Decisão do gate

**BF1-G aprovado.** BF2 pode ser planejada a partir deste checkpoint, mas permanece não iniciada. A próxima migration livre continua sendo reservada para o cache/arestas estruturais de BF2.
