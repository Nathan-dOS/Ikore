# Plano RS1–RS2 — Resolução estrutural Brownfield

Data: 2026-08-31. Branch `gpui-migration`. Baseline: `b980e73` (BF5-G). Estado: **aprovado para implementação; RS1 não iniciado**.

## Objetivo

Transformar referências AST observadas em relações determinísticas entre módulos e símbolos de projetos Rust, TypeScript e TSX. A resolução não usa LLM, compilador, LSP, package manager nem execução de scripts. Um destino só é aceito quando as regras estruturais encontram exatamente um candidato; ambiguidade e ausência de evidência continuam explícitas.

## Sequência e gates

| Fase | Entrega | Gate |
|---|---|---|
| RS1 | Contratos v2, V17, índice de símbolos, resolvedor incremental e consumidores do grafo | Precisão/recall de 100% no conjunto declarado como suportado; zero falso `resolved`; cold/warm/refresh cercados |
| RS2 | Expansão 1-hop revalidada para PreSpec/Specification | Benefício material conforme regra BF5, sem regressão; default permanece desligado até decisão posterior |

## Invariantes

- ADR-008 e BF1–BF5 permanecem concluídas; esta evolução é registrada na ADR-009.
- Brownfield continua opt-in, não destrutivo e subordinado ao trabalho principal.
- Rust, TypeScript e TSX são o único escopo sintático; demais linguagens mantêm fallback lexical.
- Resolução é feita a partir de descritores AST normalizados, nunca por coincidência de filename.
- Dispatch de instância sem tipo demonstrável, glob não único e demais casos duvidosos nunca viram destino resolvido.
- Contexto de tarefa usa somente arestas completas, com hash de origem, destino e manifestos revalidado contra bytes atuais.

## Limites

- Resolução: até 20.000 arestas e 16 candidatos por referência por lote, com cursor persistido.
- Tarefa: até 256 arestas candidatas, um hop e o teto existente de 12 fontes do `CollectionBudget`.
- O orçamento temporal existente continua obrigatório; esgotamento produz estado parcial retomável.

## Versionamento

1. `docs(brownfield): define structural resolution RS1-RS2`
2. `feat(brownfield): normalize semantic references and add V17`
3. `feat(brownfield): resolve structural relations incrementally`
4. `test(brownfield): record RS1 resolution gate`
5. `feat(brownfield): expand task context through resolved graph`
6. `test(brownfield): record RS2 benefit gate`

Cada checkpoint de implementação deve compilar e passar seus testes direcionados. RS1-G e RS2-G exigem workspace tests, Clippy estrito, formatação, build release, upgrades V1–V17 e benchmark release.
