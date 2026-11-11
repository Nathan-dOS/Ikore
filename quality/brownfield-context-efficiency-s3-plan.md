# Plano S3 — Eficiência do contexto estrutural brownfield

Data: 2026-09-01. Branch `gpui-migration`. Baseline: `6860382` (RS2-G). Estado: **aprovado para implementação**.

## Objetivo

Preservar o ganho relacional de **7/7** do RS2 e reduzir o aumento agregado de tokens de entrada de **26,59%** para no máximo **5%**. O grafo continua determinístico, revalidado e fail-closed; S3 muda somente quanto da evidência validada precisa cruzar a fronteira do prompt.

## Diagnóstico

O `StageHandoffV1` persiste corretamente o `CodeContextFacts` completo, porém a mesma representação também é serializada integralmente para o modelo. Fontes adicionadas pelo grafo repetem `projectId`, path e SHA-256 em cada snippet, símbolo e referência, além de transportar detalhes internos do extrator e da resolução que não ajudam a Specification.

## Estratégia

- Preservar integralmente o handoff persistido, sua validação, hashes, fencing e compatibilidade.
- Gerar uma projeção exclusiva para o prompt, com versão explícita e uma tabela deduplicada de fontes.
- Transportar somente:
  - path e SHA-256 de cada fonte atual;
  - snippets revalidados com intervalo de linhas;
  - símbolos que aparecem nos snippets ou participam de relações resolvidas;
  - relações `resolved` cujas duas pontas estão no pacote;
  - contagens de referências `ambiguous`, `external` e `unresolved`;
  - estado, truncamento e diagnósticos acionáveis de cobertura.
- Manter `resolutionContext` no contrato persistido e na validação do host, mas fora do prompt: o hash interno não oferece informação acionável ao modelo e a projeção já contém os hashes atuais das fontes.
- Nunca transformar uma referência ambígua ou não resolvida em relação do prompt.
- Usar o tamanho da projeção, e não o envelope persistido, ao reservar o orçamento do contexto complementar do runtime.
- Manter o conteúdo normativo de artifacts, respostas, esclarecimentos e feedback sem compactação semântica.

## Gate S3

- Qualidade `7/7` em todas as tarefas relacionais do RS2.
- Caso ambíguo sem expansão e controle simples sem regressão.
- Tokens de entrada P50 agregados: aumento máximo de 5% contra on-demand.
- Latência de Specification P50: aumento máximo de 10%.
- Mesmos hashes, provider loopback, ordem contrabalanceada e cinco pares medidos; warmup excluído.
- Testes devem provar que os bytes atuais, paths e hashes necessários chegam ao prompt, enquanto metadados repetidos não chegam.

## Ativação

Se o gate completo passar, a expansão estrutural será considerada o caminho padrão **somente para projetos brownfield com consentimento explícito e grafo completo/atual**. Ausência ou revogação de consentimento, grafo parcial/stale, ambiguidade ou orçamento insuficiente continuam usando o fallback on-demand. S3 não autoriza descoberta automática sem consentimento.

## Versionamento

1. `docs(brownfield): define context efficiency S3`
2. `feat(brownfield): compact resolved context for prompts`
3. `test(brownfield): record S3 efficiency gate`

O commit de implementação deve passar testes direcionados, formatação e Clippy. O gate final exige workspace tests, Clippy estrito, build release, upgrades V1–V17 e benchmark S3 release.
