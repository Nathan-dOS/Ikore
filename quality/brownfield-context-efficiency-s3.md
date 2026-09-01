# Gate S3 — Eficiência do contexto estrutural brownfield

Data: 2026-09-01  
Experimento: `s3-context-efficiency-v1`  
Fixture relacional preservado: `rs2-relational-tasks-v1`  
Relatório bruto: `quality/brownfield-context-efficiency-s3.json`

## Resultado

O gate S3 **foi aprovado**. A expansão pelo grafo manteve qualidade **7/7** em todas as tarefas relacionais e reduziu o aumento agregado de tokens de entrada observado no RS2 de **26,59%** para **4,948%**. A latência P50 agregada aumentou **6,887%**, dentro do teto de 10%.

Consequência: o grafo resolvido passa a ser o caminho padrão de contexto para projetos brownfield que tenham **consentimento explícito**, grafo completo e fingerprint atual. O consentimento do onboarding não foi removido nem inferido; projetos sem autorização continuam no fluxo on-demand.

## Método

- Cinco pares medidos por caso e um par de warmup excluído.
- 72 execuções isoladas no total.
- Ordem contrabalanceada entre `on_demand` e `resolved_graph`.
- Mesmos hashes de fixture, provider loopback determinístico e política de resposta nos dois modos.
- Worker real do `AppCore`, SQLite real, resolução real e releitura pelo `RepositoryContext`.
- Tokens simulados pela mesma regra do RS2: caracteres do prompt divididos por quatro; não são estimativa de preço nem tokenizer de um provider comercial.

## Evidência funcional e custo

| Caso | Qualidade | Fontes | Tokens P50 |
|---|---:|---:|---:|
| Callee sem path explícito | 3/7 → 7/7 | 1 → 2 | 1.759 → 1.882 |
| Call chain Rust | 3/7 → 7/7 | 2 → 3 | 1.826 → 1.984 |
| Alias + reexport TypeScript | 3/7 → 7/7 | 1 → 2 | 1.760 → 1.846 |
| `extends` + `implements` | 3/7 → 7/7 | 1 → 2 | 1.785 → 1.946 |
| Referência ambígua | 7/7 → 7/7 | 1 → 1 | 1.797 → 1.797 |
| Controle simples | 7/7 → 7/7 | 1 → 1 | 1.744 → 1.744 |

- Tokens de entrada P50 agregados: **+4,948%**; teto: 5%.
- Latência de Specification P50 agregada: **+6,887%**; teto: 10%.
- Perguntas de esclarecimento: zero nos dois modos.
- Regressões de qualidade: zero.
- Ampliações no caso ambíguo: zero.
- Casos sem expansão útil: custo de tokens idêntico ao baseline.

## Mudanças que produziram o ganho

- O `CodeContextFacts` completo continua persistido e validado, mas não é mais serializado integralmente no prompt.
- Uma projeção `brownfield-context-s3-v1` deduplica fontes e envia path/SHA-256 uma única vez.
- Snippets mantêm bytes atuais e intervalos de linha.
- Símbolos ficam limitados aos que aparecem nos snippets ou participam de uma relação resolvida.
- Somente relações `resolved` com as duas pontas presentes viram relações do prompt; demais estados aparecem apenas como contagens.
- `resolutionContext` permanece na validação interna, sem gastar tokens do modelo.
- Dependências puramente modulares são podadas quando já existe destino semântico exato, call ou relação de tipo. Elas continuam elegíveis quando forem a única evidência disponível.
- O orçamento do runtime passa a considerar o tamanho da projeção que realmente será enviada, sem truncar conteúdo normativo.

## Decisão de ativação

Estado: `resolved_graph_default_after_project_consent`.

O comportamento padrão agora é:

1. sem consentimento brownfield: coleta on-demand existente;
2. com consentimento, mas grafo ausente, parcial ou stale: fallback on-demand e refresh em background;
3. com consentimento e grafo completo/atual: expansão semântica compacta por padrão;
4. referência ambígua ou sem destino único: nenhuma expansão por essa relação;
5. orçamento esgotado: pacote parcial cercado, sem promover conteúdo cacheado a snippet.

Esta decisão ativa o uso eficiente do grafo; não altera os seis estágios da pulsação, o fencing, a política de leitura segura ou o modelo de consentimento do onboarding.
