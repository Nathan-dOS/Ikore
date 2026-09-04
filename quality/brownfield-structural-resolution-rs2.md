# Gate RS2 — Benefício do grafo estrutural nas tarefas

Data: 2026-08-31  
Fixture: `rs2-relational-tasks-v1`  
Relatório bruto: `quality/brownfield-structural-resolution-rs2.json`

## Resultado

O gate combinado **não foi aprovado para ativação geral**. A expansão por grafo produziu benefício material e determinístico de qualidade nas quatro tarefas relacionais, sem falsos destinos no caso ambíguo e sem regressão de qualidade no controle simples. Entretanto, o custo agregado de tokens de entrada aumentou **26,59%**, acima do limite simétrico de regressão de 5% definido para o gate.

Consequência: a implementação RS2 permanece disponível apenas sob consentimento brownfield e continua desativada por padrão. Uma ativação geral continua exigindo decisão explícita e não faz parte deste commit.

## Método

- Cinco pares medidos por caso e um par de warmup excluído.
- Ordem contrabalanceada entre `on_demand` e `resolved_graph` por caso/iteração.
- Repositórios isolados com fingerprints idênticos em cada par.
- Mesmo provider loopback determinístico, com atraso fixo de 10 ms e a mesma política de resposta nos dois modos.
- Worker real do `AppCore`, SQLite real, mapeamento/resolução real e releitura pelo `RepositoryContext`.
- Tempo do mapeamento foi excluído da latência da tarefa; a medição começa na criação da pulsação.
- Tokens são a métrica simulada já usada nos gates anteriores (`caracteres do prompt / 4`), não uma estimativa de preço.

## Evidência funcional

| Caso | On-demand | Grafo resolvido | Resultado |
|---|---:|---:|---|
| Callee sem path explícito | 3/7 | 7/7 | `auth.rs`, revisão atual e `validate_token` incluídos |
| Call chain Rust | 3/7 | 7/7 | `policy.rs`, revisão atual e `apply_rule` incluídos |
| Alias + reexport TypeScript | 3/7 | 7/7 | `token.ts`, barrel e símbolo `verify` incluídos |
| `extends` + `implements` | 3/7 | 7/7 | `contracts.ts` e contrato atual incluídos |
| Referência ambígua | 7/7 | 7/7 | uma fonte; zero referências promovidas a resolved; aresta `ambiguous` confirmada no banco |
| Controle simples | 7/7 | 7/7 | uma fonte nos dois modos; sem regressão de qualidade |

Todas as amostras com grafo usaram um `resolutionContext` completo e atual. O provider marcou a Specification como fundamentada somente quando os bytes atuais do alvo estavam no prompt; isso ocorreu em 100% das amostras relacionais com grafo e em 0% dos respectivos controles on-demand.

## Custo e decisão

- Tokens de entrada P50 agregados: **+26,59%**.
- Latência de Specification P50 agregada: **+7,29%**.
- Perguntas de esclarecimento: zero nos dois modos.
- Regressões de qualidade: zero.
- Ampliações no caso ambíguo: zero.
- Controle simples: qualidade preservada; uma fonte em ambos os modos.

Regra aplicada: benefício por qualidade, menos perguntas, redução de tokens de pelo menos 5% ou redução de latência de pelo menos 10%, sem regressão; aumentos superiores a 5% em tokens ou 10% em latência reprovam o gate combinado.

Decisão registrada: **benefício de qualidade comprovado, gate combinado reprovado por custo de tokens, default inalterado**.

## Observação de robustez descoberta pelo gate

O fixture ambíguo revelou uma colisão de `symbolKey` para declarações homônimas com assinatura idêntica. A extração agora preserva a chave anterior para símbolos únicos e deriva chaves distintas e determinísticas do intervalo e da ordem da declaração somente quando existe colisão. Isso impede que código parcial ou ambíguo aborte a persistência V17 e permite que o resolvedor permaneça fail-closed.

A repetição do BF5 em modo debug também revelou que o nome sintético `token.ts` era barrado corretamente pela política de fontes sensíveis e que o teste observava apenas o primeiro lote quando o limite de 2 s era atingido. O fixture foi renomeado para `auth-validator.ts`, e o harness agora aguarda a retomada completa dos lotes sem enfraquecer a política de segurança nem os limiares de precisão/recall.
