# Checkpoint BF4 — Manutenção e indexação ampliada

Data: 2026-08-30. Branch `gpui-migration`. Baseline: `5d1dc66` (BF3-G). Estado: **BF4-G aprovado; BF5 não iniciada**.

## Resultado

BF4 substitui a manutenção brownfield por uma fronteira persistida e derivada. O primeiro mapeamento continua sendo o único scan estrutural completo; depois da inicialização, mudanças chegam por eventos exatos do file watcher e a fila processa somente `(project_id, relative_path, content_sha256)` alterados, stale ou removidos. Eventos anteriores ao bootstrap não ocultam fontes irmãs, e revisões repetidas são deduplicadas.

A prioridade da fronteira combina uso recente, in-degree recalculado e envelhecimento. Uso observado no PreSpec aumenta a prioridade do módulo, enquanto `age_factor` permite que entradas antigas eventualmente avancem. Trabalho principal pendente, slot principal ocupado ou publicação principal sempre impede admissão de mapeamento e embeddings de manutenção.

Notas `type: component` em `knowledge/obsidian/components/` agora participam do pipeline vetorial M4 por delta exato, sem se tornarem documentos normativos. Publicação, edição externa e exclusão enfileiram somente a path afetada; exclusão remove o vetor; mudança de modelo invalida vetores do modelo anterior; resultado tardio continua cercado pelo hash atual.

## Evidências do escopo BF4

| Item | Evidência |
|---|---|
| BF4.1 — migration aditiva | V16 cria `brownfield_frontier` com os sete estados aprovados, in-degree, razão de prioridade, uso recente, envelhecimento, orçamento e tentativas. `brownfield_frontier_state` distingue bootstrap ainda não executado de fronteira vazia, e `brownfield_refresh_queue` persiste a deduplicação por projeto/path/revisão. |
| BF4.2 — fronteira derivada | Após aceitar fatos estruturais, `recompute_brownfield_frontier_tx` recalcula in-degree contando arestas `resolved`; remoção/substituição de arestas reduz o valor em vez de acumular incrementos. Cache V14 existente é migrado sem nova leitura do filesystem. |
| BF4.3 — refresh exato | `MaintenanceWorkspaceEventSink` persiste o evento antes de publicá-lo à UI. Após o bootstrap, `brownfield_map` consome exclusivamente a fila de refresh; hash inalterado é descartado, revisão concorrente supersede a antiga, rename/delete afetam somente as paths observadas e revogação de consentimento encerra a fila. |
| BF4.4 — embeddings delta | O classificador de embeddings aceita documentos normativos ou notas `type: component` na pasta de componentes. `enqueue_embedding_path_change` coalesce alterações por path/hash; publicação de componente e watcher alimentam o delta; remoção apaga vetor; troca de modelo remove vetores antigos; scan periódico incondicional foi eliminado. |
| BF4.5 — uso + envelhecimento | Fontes efetivamente usadas no PreSpec atualizam `recent_use_count` e `last_used_at`. A admissão ordena `recent_use*100 + in_degree*10 + age_factor`, com uso limitado e envelhecimento crescente para impedir concentração permanente. Chamadas de enrichment cobradas também incrementam o orçamento da fronteira das fontes candidatas. |
| BF4.6 — ignore persistido | Contratos core/UI permitem ignorar ou reativar a fonte selecionada. `ignored` é persistido na V16, cancela refresh pendente e é respeitado por eventos futuros. Reativação captura a revisão corrente e a recoloca na fila sem scan amplo. |
| BF4.7 — matriz | Testes cobrem bootstrap frio sem rescan ocioso, evento antes do bootstrap, deduplicação, rename/delete, substituição e remoção de arestas, in-degree, ignore/reativação, uso recente versus aging, watcher, prioridade principal, delta de componente, exclusão de vetor, troca de modelo e resultado vetorial tardio. |

## Persistência e lifecycle

- `V16__brownfield_frontier.sql` é aditiva e não modifica V1–V15.
- `brownfield_frontier` mantém uma linha por projeto/path; seus valores derivados são substituídos transacionalmente.
- `brownfield_frontier_state` impede que um evento precoce seja confundido com bootstrap completo.
- `brownfield_refresh_queue` usa chave `(project_id, relative_path, content_revision_key)`; exclusão usa uma revisão sentinela e é idempotente.
- O watcher só amplia a observação para extensões de código elegíveis e notas `.md`/`.txt`; o editor continua restrito a documentos, e o leitor seguro permanece responsável por excludes, segredos, binários e escapes.
- Recovery devolve refresh `in_progress` para um estado retomável; consentimento revogado supersede refresh pendente e mantém cache já conhecido.
- A transação de gravação de rotas LLM passou a reservar o escritor com `IMMEDIATE`, evitando a corrida read-to-write exposta pela concorrência adicional do watcher.

## Invariantes e limites

- O filesystem só é percorrido integralmente no bootstrap estrutural ainda não inicializado; idle e refresh explícito não fazem scan incondicional.
- Fronteira vazia após inicialização é um estado válido e não reabre o bootstrap.
- Notas de componentes ganham embeddings, mas continuam excluídas da recuperação normativa de Specification/Contract.
- Ignore é uma decisão por fonte de código e exige consentimento brownfield ativo; eventos não a desfazem.
- BF4 não implementa barra de cobertura, benchmark comparativo nem decisão de ativação; esses itens permanecem em BF5.

## Validação

| Gate | Resultado |
|---|---|
| `cargo fmt --all -- --check` | aprovado |
| `cargo test -p hvc-core --lib` | 232 aprovados |
| `cargo test -p hvc-test-support` | aprovado, incluindo upgrades até V16 |
| `cargo test --workspace --all-targets --no-fail-fast` | 356 aprovados, 0 falhas |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | aprovado |
| `cargo build --workspace --release` | aprovado |
| `git diff --check` | aprovado |

O warning de incompatibilidade futura de `proc-macro-error2 v2.0.1` é preexistente em dependência de terceiros e não é warning de Clippy nem falha do gate.

## Decisão do gate

**BF4-G aprovado.** A fronteira é derivada e persistida; a manutenção posterior ao bootstrap não varre o repositório incondicionalmente; ignore é durável; embeddings de componentes são atualizados por delta e cercados por revisão/modelo; e trabalho do usuário mantém precedência sobre manutenção. BF5 pode iniciar a partir deste checkpoint, mas permanece não iniciada.
