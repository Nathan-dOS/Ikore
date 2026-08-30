# Checkpoint BF3 — Enriquecimento semântico gradual

Data: 2026-08-30. Branch `gpui-migration`. Baseline: `4994b4f` (BF1-G e BF2-G consolidados). Estado: **BF3-G aprovado; BF4 não iniciada**.

## Resultado

BF3 transforma a compreensão já produzida por Specification em propostas documentais opcionais, sem acrescentar uma chamada LLM obrigatória ao estágio. O output estruturado ganhou `componentProposals`; o host converte no máximo duas propostas em artifacts pathless `component_proposal`, revalida cada `(project_id, relative_path, content_sha256)` no checkpoint e só então materializa notas propostas.

Notas válidas são renderizadas deterministicamente pelo host e publicadas pelo outbox em `knowledge/obsidian/components/`. Arquivo humano preexistente causa conflito registrado e permanece byte a byte intacto. Mudança posterior de fonte marca o registro `stale`, cancela publicação ainda não aplicada e não dispara regeneração imediata.

Um job opcional `brownfield_enrich` usa capacidade de background e o grafo BF2 para expandir vizinhos de importação. O raio é 1 ou 2, a recuperação é limitada a `k=2`, cada lote aceita até duas propostas e a resposta é limitada a 2.400 tokens totais, equivalentes ao teto declarado de 1.200 por nota. O orçamento é cobrado de forma durável antes da chamada. `brownfield_llm_budget=0` persiste `budget_exhausted`, mantém `budget_calls=0` e executa zero chamadas.

## Evidências do escopo BF3

| Item | Evidência |
|---|---|
| BF3.1 — subproduto de Specification | `specification_output_schema` exige `componentProposals`; `StageExecutionResult::outcome` cria wrappers V1 `type=component_proposal`, `status=proposed`, pathless e limitados a dois. O prompt proíbe fontes/links inventados. Não há segundo request no caminho do estágio. |
| BF3.2 — enriquecimento oportunista | `brownfield_enrichment.rs` agenda um job por pulsação, usa raio 1/2 e `k=2`, congela bytes/hashes seguros, cobra `budget_calls` antes da rede e aceita no máximo duas propostas. Imports não resolvidos pela BF2 só viram vizinho quando um único path já presente no cache pode ser inferido; ambiguidade falha fechada. |
| BF3.3 — settings | V15 cria defaults `brownfield_llm_budget=1` e `brownfield_expansion_radius=1`. A persistência valida orçamento 0–8 e raio 1/2; a UI permite alternar ambos. Mudar orçamento para zero cancela trabalho pendente/em voo e registra esgotamento sem chamada. |
| BF3.4 — schema de nota | Renderer host inclui `type: component`, `mapping-status: proposed`, `source-files`, mapa `observed-revision`, `generator`, `generator-version` e `confidence`. Summary fornecido pelo modelo não pode injetar wikilink. |
| BF3.5 — publicação segura | `component_notes.rs` cria artifact host `component` e outbox background. `apply_publication_unit` diferencia Learning e Component. Expected ausente impede sobrescrever nota humana; conflito é durável. Componentes são excluídos de handoffs normativos e de `is_normative_document`. |
| BF3.6 — wikilinks | O host só renderiza wikilink para componente já publicado/agendado ou presente no mesmo lote fresco. Vizinho ausente vira texto `Pendente`, sem link fictício. |
| BF3.7 — stale | `brownfield_mapping.rs` compara o hash novo com `brownfield_component_sources`. Divergência ou remoção marca a nota `stale`; publicação pendente é cancelada e nota já publicada não é reescrita. |
| BF3.8 — matriz | Testes cobrem contrato pathless e Specification-only, fonte fresca/stale, publicação, conflito humano, wikilink ausente, sanitização de summary, budget zero sem rota LLM, cobrança positiva única, vizinho estrutural, generator de enrichment, stale posterior sem regeneração e exclusão normativa. |

## Persistência e lifecycle

- `V15__brownfield_semantic_enrichment.sql` é aditiva e não modifica V1–V14.
- `brownfield_component_notes` guarda estado de mapeamento/publicação, gerador, confiança e vínculo aos artifacts.
- `brownfield_component_sources` guarda proveniência por projeto/path/hash.
- `brownfield_enrichment_runs` guarda raio, limite e consumo de orçamento, inputs/outputs, contagem de propostas e diagnóstico.
- Revogação de consentimento cancela `brownfield_map`, `brownfield_enrich` e publicações Component ainda pendentes; notas já publicadas não são apagadas.
- Shutdown/recovery preserva chamadas já consumidas e só retoma com consentimento ativo e orçamento restante.
- A fila principal e publicações principais continuam com prioridade sobre enriquecimento e publicação de Component.

## Invariantes e limites

- `proposed` não significa `validated`; BF3 não promove automaticamente uma nota.
- Specification continua com os seis estágios existentes e não consulta as tabelas BF3.
- Propostas sem fonte exata, com source stale, ID inválido ou mais de duas entradas são recusadas/omitidas de forma auditável.
- Cobertura parcial, lote vazio e ausência de vizinho são resultados válidos; não criam fatos fictícios.
- Nenhuma frontier, PageRank, file watcher, prioridade por envelhecimento ou indexação vetorial de componentes foi implementada; esses itens permanecem em BF4.

## Validação

| Gate | Resultado |
|---|---|
| `cargo fmt --all -- --check` | aprovado |
| `cargo test -p hvc-core --lib` | 222 aprovados |
| `cargo test -p hvc-test-support` | aprovado, incluindo fixtures e upgrades V15 |
| `cargo test --workspace --all-targets --no-fail-fast` | 345 aprovados, 0 falhas |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | aprovado |
| `cargo build --workspace --release` | aprovado na versão final em 5m25s |
| `git diff --check` | aprovado |

O warning de incompatibilidade futura de `proc-macro-error2 v2.0.1` é preexistente em dependência de terceiros e não é warning de Clippy nem falha do gate.

## Decisão do gate

**BF3-G aprovado.** As notas têm proveniência auditável; Specification não gasta chamada LLM adicional obrigatória; budget zero executa zero chamadas; edições humanas são preservadas; stale não regenera imediatamente; lote vazio e cobertura parcial permanecem estados válidos. BF4 pode iniciar a partir deste checkpoint, mas permanece não iniciada.
