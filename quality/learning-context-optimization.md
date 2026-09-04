# MVP — Telemetria de contexto e refinamento HITL do grafo

Data da consolidação: 2026-09-02. Branch de trabalho: `gpui-migration`.

Status: **P1 + P2 implementados e gate técnico aprovado**. O modo persistido permanece `Observe` por default; nenhuma nota é alterada sem decisões humanas individuais e publicação posterior pelo host.

## Escopo entregue

- Migration aditiva V19 com runs/itens de retrieval, avaliações Learning V2, propostas de grafo e configuração de modo, sem backfill.
- Trace tipado de pool, entrega e citação para os seis agentes. Falhas de provider, parsing, validação, cancelamento ou fence preservam a telemetria disponível.
- `LearningInputV2` e `LearningReportV2` com relatórios V1 históricos legíveis, input limitado a 16.000 caracteres e uma única chamada Learning posterior à terminalidade `Done` ou `Failed`.
- Geração pelo host de até oito candidatos normativos e operacionais. O modelo somente seleciona IDs fornecidos; paths, relação, hashes, diff e conteúdo aplicável permanecem sob autoridade do host.
- Revisão HITL por proposta, proteção de geração/clique obsoleto, composição por nota em um artifact `knowledge_graph_patch` e publicação por CAS/substituição atômica.
- Patcher textual conservador de frontmatter/Markdown, com validação YAML antes/depois e recusa de estruturas complexas.
- Snapshot e UI GPUI para métricas por estágio, taxa de citação, precisão observada heurística, propostas, evidências, diff, decisões e conflitos.
- Kill switch: `Observe` ou Learning desligado impede novas propostas e cancela publicações não iniciadas; uma aplicação atômica já iniciada termina e continua auditável.

## Evidências automatizadas

| Verificação | Resultado |
|---|---|
| `cargo fmt --all -- --check` | Aprovado |
| `cargo clippy --workspace --all-targets -- -D warnings` | Aprovado |
| `cargo test --workspace --locked --offline`, com `RUST_TEST_THREADS=1` | **395 testes aprovados**, zero falhas e zero ignorados |
| `cargo build --release -p hvc-app --locked --offline` | Aprovado |
| `scripts/smoke-windows.ps1` | Aprovado; banco descartável, shutdown gracioso e exit code 0 |
| `scripts/gate-stage7.ps1`, com `RUST_TEST_THREADS=1` | **Gate técnico aprovado**: estrutura GPUI/Rust, fmt, Clippy, testes, release, smoke, supply chain e advisories |
| Auditoria RustSec | Exit code 0, zero vulnerabilidades; 10 avisos `unmaintained`, 2 `unsound` e 1 `yanked` informativos |
| Auditoria de licenças/fontes | 551 pacotes resolvidos, zero licença negada, zero metadado ausente e zero fonte pendente de revisão |

O build ainda informa a futura incompatibilidade já conhecida de `proc-macro-error2 2.0.1`.

Uma execução paralela completa expôs duas oscilações temporais nos testes V18 de resolução Brownfield, cujo orçamento de coleta é de dois segundos. Ambos passaram imediatamente isolados. A execução autoritativa completa e o gate Stage 7 foram repetidos serialmente e passaram sem testes pulados. A estabilização de V18 continua sendo a premissa separada registrada no plano.

## Segurança e autoridade verificadas

- Ausência de citação não é convertida automaticamente em inutilidade; avaliações podem permanecer `indeterminate`.
- IDs de candidato/evidência desconhecidos, recorrência falsa e proposta sem evidência operacional são rejeitados.
- Não há escrita antes do HITL. Múltiplas propostas para a mesma nota produzem uma única publicação.
- Alteração humana entre proposta e aplicação produz `stale`/`conflict` e preserva o arquivo existente.
- Publicações de pulsações falhas aguardam base `ready`; trabalho principal e preempção mantêm prioridade.
- Após publicação, o cache da nota é invalidado e seu delta de embedding é enfileirado quando o índice está habilitado.
- Recovery após restart preserva publicações aprovadas e permite concluir uma aplicação já iniciada pelo protocolo atômico.

## Condição de distribuição

O gate técnico passou, mas `distributionAllowed` permanece `false` pela política existente: o issue upstream `zed-55470`, sobre GPUI e dependências transitivas GPL, segue marcado como bloqueador de distribuição até resolução upstream ou decisão jurídica explícita. Isto não foi introduzido por este MVP.

## Limites mantidos

Ficam fora deste MVP: P3/few-shot cache, P4/reranking por prompt, aplicação automática, remoção de relações, alteração de `risk`/`status` e reescrita autônoma de notas. Relatórios Learning continuam não normativos e excluídos do retrieval.

Decisão arquitetural: [ADR-008](<home>/Documents/Nathan/HardVibeCode/aplicativo/Projeto%20Hard-Code/Documentos/02-Arquitetura/ADRs/ADR-008-Telemetria-de-Contexto-e-Refinamento-HITL-do-Grafo.md).
