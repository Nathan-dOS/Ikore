# Checkpoint BF2 — Extratores e Grafo Estrutural

Data: 2026-08-29. Branch `gpui-migration`. Baseline: `37d99b6` (M5 funcional e smoke visual concluídos), com BF1-G preservado no mesmo worktree. Estado: **BF2-G aprovado; BF3 não iniciada**.

## Resultado

BF2 adiciona extração estrutural local e versionada para Rust, TypeScript e TSX. O contrato consumido por PreSpec/Specification continua sendo `CodeContextFacts`: os consumidores não conhecem Tree-sitter, cache ou tabelas de grafo. Linguagens sem gramática BF2 permanecem disponíveis pelo fallback lexical e erros sintáticos geram cobertura parcial, não falha catastrófica.

Após o inventário explicitamente autorizado de BF1, um job `brownfield_map` sem pulsação é enfileirado no slot de manutenção existente. O job respeita consentimento, projeto, geração, cancelamento e `CollectionBudget`; não cria pulsação e não chama LLM/embedding.

## Evidências do escopo BF2

| Item | Evidência |
|---|---|
| BF2.1 — adaptador | `TreeSitterExtractor` implementa `CodeContextExtractor` sobre `SourceSnapshot[]`. `CodeContextFacts` ganhou `StructuredSymbol` e relação tipada de referência, com defaults Serde para artefatos V1 anteriores. |
| BF2.2 — gramáticas/queries | Dependências fixadas: `tree-sitter=0.26.13`, `tree-sitter-rust=0.24.2` e `tree-sitter-typescript=0.23.2`. Queries `rust-structure-v1` e `typescript-structure-v1` capturam funções/métodos, structs/classes, traits/interfaces, enums, aliases, imports e chamadas. |
| BF2.3 — migration | Apenas `V14__brownfield_structure.sql` foi adicionada. `brownfield_source_cache` e `brownfield_edges` usam chaves com `project_id`; V1–V13 não foram editadas. `jobs.maintenance_metrics_json` registra métricas operacionais locais do job. |
| BF2.4 — revisão/arestas | Cada miss substitui cache e todas as arestas da fonte numa transação `IMMEDIATE`, cercada por job/projeto/geração/consentimento. A revisão é o SHA-256 do conteúdo completo; mudança apenas no corpo invalida. Não existe contador de in-degree persistido: ele é derivável por `COUNT` das arestas atuais. |
| BF2.5 — roteamento | `RoutedCodeContextService` preserva a descoberta/ranking seguro e delega os snapshots selecionados ao router Tree-sitter. Extensões `.rs`, `.ts` e `.tsx` usam gramática; demais extensões usam fallback lexical. |
| BF2.6 — PreSpec | `execute_preparation` e a revalidação sob demanda de Specification usam `RoutedCodeContextService`. O pacote e o handoff continuam serializando o mesmo `CodeContextFacts`; Specification permanece agnóstica ao extrator. |
| BF2.7 — orçamento/cancelamento | Enumeração, extração e manutenção verificam `CancellationToken`, bytes, arquivos e duração. Esgotamento devolve `CoverageState::Partial` com diagnóstico `budget_exhausted`; manutenção parcial preserva cache não visitado. Revogação cancela jobs pendentes/em execução e cerca commits tardios. Shutdown devolve o job à fila; recovery também retoma uma execução interrompida, somente se o consentimento do projeto continuar ativo. |
| BF2.8 — matriz | Testes cobrem Rust/TypeScript, fallback, erro sintático parcial, cold/warm com `cache_hits`, `cache_misses` e `elapsed_us`, hash do corpo, parser/query versionados, isolamento de mesma path entre projetos, substituição de calls, revogação, retomada após shutdown e upgrade V9→V14. |

## Invariantes e limites

- Cache estrutural é identificado por projeto/path e validado por hash do conteúdo, versão do parser e versão da query.
- Cache hit não executa parser; cache miss registra tempo de parse e substitui as arestas da revisão anterior.
- Exclusões só removem cache antigo quando a enumeração e o processamento terminaram com cobertura completa.
- Arestas BF2 permanecem observadas e não resolvidas; resolvedores de módulos multilíngues ficam fora desta fase.
- Nenhuma nota de componente, frontier, PageRank, enriquecimento semântico ou chamada LLM foi adicionada.
- O cache persistente da manutenção e o contexto sob demanda compartilham contratos/versões, mas Specification nunca consulta tabelas BF2 diretamente.
- Consentimento revogado impede novos claims e commits tardios; dados já mapeados não são apagados implicitamente.
- Interrupções de shutdown não consomem definitivamente o job: o caminho normal o recoloca como `pending`, e o recovery cerca a retomada pelo consentimento ainda ativo.

## Validação

| Gate | Resultado |
|---|---|
| `cargo fmt --all -- --check` | aprovado |
| `cargo test -p hvc-core` | 215 aprovados |
| `cargo test -p hvc-core brownfield_mapping --no-fail-fast` | 4 aprovados |
| `cargo test -p hvc-core tree_sitter_context --no-fail-fast` | 5 aprovados |
| `cargo test --workspace --all-targets --no-fail-fast` | 338 aprovados, 0 falhas na repetição integral |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | aprovado |
| `cargo build --workspace --release` | aprovado |
| `git diff --check` | aprovado |

A primeira execução integral encontrou apenas a expectativa legada `MAX(version)=13` no teste de upgrade. A expectativa foi atualizada para V14; o alvo isolado passou 5/5 e a repetição integral terminou 338/338. O warning de incompatibilidade futura de `proc-macro-error2 v2.0.1` é uma dependência preexistente e não é warning de Clippy nem falha do gate.

## Decisão do gate

**BF2-G aprovado.** Rust/TypeScript produzem `CodeContextFacts` estruturados; demais linguagens usam fallback lexical; cache invalida por hash/parser/query; arestas são substituídas por revisão. BF3 pode iniciar a partir deste checkpoint, mas permanece não iniciada.
