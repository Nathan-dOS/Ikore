# Checkpoint BF5 — Cobertura, benchmarks e ativação

Data: 2026-08-31. Branch `gpui-migration`. Baseline: `a7e09fc` (BF4-G). Estado: **BF5-G aprovado; rollout Brownfield concluído em modo opt-in**.

## Resultado

BF5 torna a cobertura brownfield mensurável e auditável sem acrescentar migration. O runtime deriva as métricas das tabelas V14–V16 e as entrega no mesmo snapshot periódico das Pulsações. O denominador é explícito: fontes conhecidas na fronteira menos fontes `ignored`. Extração válida conta somente `mapped`; `partial`, `stale` e `failed` permanecem visíveis e não inflam o progresso.

A documentação é contada por fonte elegível distinta, não por quantidade de notas. Cada fonte ocupa um único estado, com precedência conservadora `stale > validated > proposed`. Alterações humanas no frontmatter `mapping-status` de uma nota Component agora sincronizam o estado persistido; exclusão da nota a marca `stale`. A UI mostra porcentagem, barra, numerador/denominador, estados de extração, estados documentais e fontes ignoradas no painel de Pulsações.

O benchmark BF5 usa o AppCore, worker, watcher e SQLite reais com provider loopback, repositórios temporários equivalentes e ordem contrabalanceada. O artefato integral está em [brownfield-benchmark-bf5.json](C:/Dev/Hard-Vibe-Code/quality/brownfield-benchmark-bf5.json), e pode ser regenerado por:

```powershell
cargo run --release -p hvc-test-support --example brownfield_benchmark -- quality/brownfield-benchmark-bf5.json 5
```

## Evidências do escopo BF5

| Item | Evidência |
|---|---|
| BF5.1 — métricas | `BrownfieldCoverageMetrics` expõe bootstrap, inventário conhecido, denominador elegível, ignoradas, extração válida/parcial/stale/falha, progresso em basis points e documentação proposta/validada/stale. A consulta agrega fontes distintas e impede sobreposição de estados documentais. |
| BF5.2 — progresso visual | O snapshot de Pulsações carrega cobertura somente com consentimento ativo. O painel renderiza barra limitada a 0–100%, `mapped/eligible`, a definição textual do denominador e todos os estados que explicam cobertura incompleta. |
| BF5.3 — corpus | Fixture release com 58 entradas estruturais observadas: 17 Rust, 16 TypeScript, 16 TSX e 9 fallback/unsupported (inclui 8 Python e o manifesto). Cinco amostras válidas, além de um warmup por variante. |
| BF5.3 — cold/warm/invalidação | Cold: P50 902,479 ms, P95 1.615,859 ms para 58 entradas. Warm explícito: P50 8,440 ms, P95 41,684 ms e zero fontes processadas. Edição observada pelo watcher: P50 51,711 ms, P95 63,324 ms e exatamente uma fonte processada. |
| BF5.3 — parsing | P95 por fonte: Rust 3,397 ms; TypeScript 6,697 ms; TSX 7,540 ms; fallback 0,004 ms. O relatório preserva cada amostra de parsing para auditoria. |
| BF5.3 — resolução | O corpus produziu 112 referências: 0 resolvidas, 112 não resolvidas, 0 externas. A razão 0% é registrada como limitação real; nenhum destino foi inferido ou inventado. |
| BF5.4 — comparação | Mesma fixture inicial, mesmo hash do repositório no início da tarefa, mesma descrição, mesmo provider e mesmos limites. `empty_vault` e `partial_coverage` obtiveram qualidade 5/5, zero perguntas, 5.394 tokens de entrada simulados e 160 de saída. Latência P50 até o checkpoint Specification: 733,788 ms vs 728,573 ms (−0,71%); P95: 1.435,698 ms vs 786,584 ms. |
| BF5.5 — calibração | Mantidos os limites conservadores: 2.000 arquivos, 16 MiB totais, 1 MiB por arquivo e 2.000 ms. O cold P95 ficou dentro do teto e o refresh exato ficou muito abaixo; o corpus não oferece evidência para reduzir os limites de segurança ou ampliá-los para repositórios maiores. |
| BF5.6 — ativação | A regra exige benefício material (qualidade maior, menos perguntas, ≥5% menos tokens ou ≥10% menor latência) sem regressão simétrica. Houve igualdade de qualidade, perguntas e tokens, e ganho P50 de apenas 0,71%. Resultado: `keep_opt_in_disabled_by_default`. |

## Semântica das métricas

- `inventory_known_sources`: linhas atualmente conhecidas em `brownfield_frontier`, inclusive ignoradas.
- `eligible_source_denominator`: `inventory_known_sources - ignored_sources`; é o único denominador da barra.
- `extracted_valid_sources`: somente fronteira `mapped`; fallback/unsupported permanece `partial`.
- progresso vazio é 0%, inclusive após bootstrap de um repositório sem fontes; não declara conclusão sem evidência.
- documentação agrega `brownfield_component_sources` por path elegível. Uma fonte coberta por várias notas ainda conta uma vez.
- se qualquer nota da fonte está stale, a fonte documental é stale; caso contrário, validated precede proposed.
- consentimento ausente ou revogado não expõe cobertura no snapshot de UI e não ativa mapeamento.

## Benchmark release

| Medida | Vault vazio | Cobertura parcial |
|---|---:|---:|
| Pares válidos | 5 | 5 |
| Latência até Specification P50 | 733,788 ms | 728,573 ms |
| Latência até Specification P95 | 1.435,698 ms | 786,584 ms |
| Tokens de entrada simulados P50/P95 | 5.394 / 5.394 | 5.394 / 5.394 |
| Tokens de saída simulados P50/P95 | 160 / 160 | 160 / 160 |
| Qualidade determinística | 5/5 em todas | 5/5 em todas |
| Perguntas de esclarecimento | 0 | 0 |

Tokens são a telemetria simulada do fixture (`chars/4`), não tokenização nem estimativa de custo de provider real. A latência é observação monotônica do início da criação até o checkpoint persistido. O P95 de cinco amostras é informativo, não base isolada para rollout; a decisão usa o conjunto de sinais e a regra registrada.

O benchmark também confirma uma limitação arquitetural: a coleta segura do Specification continua on-demand e não consome o cache estrutural brownfield. Por isso, igualdade de qualidade e tokens é esperada nesta versão. Integrar o cache ao caminho crítico exigirá desenho próprio, cercas de revisão e novo benchmark; BF5 não presume esse benefício.

## Validação

| Gate | Resultado |
|---|---|
| benchmark BF5 release, 5 pares + warmup | aprovado; relatório JSON persistido |
| equivalência por hash inicial e no instante da tarefa | aprovada em todos os pares |
| `cargo fmt --all -- --check` | aprovado |
| `cargo test --workspace --all-targets --no-fail-fast` | 360 aprovados, 0 falhas |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | aprovado |
| `cargo build --workspace --release` | aprovado |
| `git diff --check` | aprovado |

O warning de incompatibilidade futura de `proc-macro-error2 v2.0.1` é preexistente em dependência de terceiros e não é warning de Clippy nem falha do gate.

## Decisão do gate

**BF5-G aprovado.** As métricas distinguem conhecimento, elegibilidade, validade e documentação sem denominador oculto; a UI explica a cobertura; cold/warm/refresh e os dois cenários de tarefa são reproduzíveis; os limites permanecem fechados; e a ausência de benefício material mantém o Brownfield **desativado por padrão e disponível somente por consentimento explícito**.

BF1–BF5 estão concluídas. Uma futura ativação automática não está autorizada por este gate e exige nova evidência, especialmente resolução estrutural útil e integração cercada do cache com o contexto da tarefa.
