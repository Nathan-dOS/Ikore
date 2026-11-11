# Checkpoint RS1 — Resolução estrutural Brownfield

Data: 2026-08-31. Branch `gpui-migration`. Baseline do runtime: `7205acf`. Estado: **RS1-G aprovado; Brownfield permanece opt-in e desativado por padrão**.

## Resultado

O gate RS1 valida o resolvedor determinístico e persistido de relações Rust, TypeScript e TSX. O runtime usa descritores normalizados extraídos da AST, só grava `resolved` quando existe um único destino, classifica imports externos, preserva ambiguidade e mantém casos sem prova como `unresolved`. Não há fallback por coincidência de filename, LLM, compilador, LSP, package manager ou execução de configuração.

O pipeline de background agora é `brownfield_map → brownfield_resolve → brownfield_enrich`. A resolução é cercada por consentimento, geração, cancelamento e `resolution_context`; pode retomar por cursor após 20.000 arestas ou dois segundos. Mudanças de código reextraem somente a fonte observada e reavaliam o grafo pelo cache. Mudanças de `Cargo.toml`, `package.json`, `tsconfig.json` e `jsconfig.json` invalidam somente a resolução.

O relatório integral e reproduzível está em [brownfield-structural-resolution-rs1.json](../quality/brownfield-structural-resolution-rs1.json). Comando:

```powershell
cargo run --release -p hvc-test-support --example brownfield_benchmark -- quality/brownfield-structural-resolution-rs1.json 5
```

## Oracle de precisão

| Classificação | Quantidade | Resultado |
|---|---:|---|
| Relações observadas | 148 | corpus Rust/TS/TSX e negativos rotulados |
| `resolved` esperado | 98 | 98 destinos e símbolos corretos |
| `external` | 32 | React e built-ins reconhecidos sem destino interno |
| `ambiguous` | 1 | dois bindings exatos; nenhum destino promovido |
| `unresolved` | 17 | dispatch de instância/namespace externo sem prova |
| Falsos `resolved` | 0 | gate fechado |
| Precisão | 100,00% | 10.000 basis points |
| Recall suportado | 100,00% | 10.000 basis points |
| Mismatches de classificação | 0 | oracle integral satisfeita |

As golden fixtures cobrem módulos `crate/self/super`, grupos, aliases, globs e reexports Rust; imports relativos, `baseUrl/paths`, default/named/namespace, aliases e reexports TS; pacotes locais Cargo/npm; calls locais/importadas, membros estáticos, `extends`, `implements`; pacote externo; ambiguidade; e método de instância não demonstrável.

## Lifecycle e desempenho

| Medida release | P50 | P95 | Gate |
|---|---:|---:|---|
| Cold mapping, 62 fontes | 1.032,493 ms | 1.052,009 ms | abaixo de 2.000 ms |
| Warm mapping | 22,146 ms | 44,785 ms | zero fontes relidas |
| Refresh exato | 49,756 ms | 64,198 ms | exatamente uma fonte |

- O warm run não criou outro job de resolução e reutilizou o fingerprint completo.
- O refresh alterou somente `src/auth.rs`; a reavaliação estrutural restante ocorreu sobre o cache.
- A amostra representativa resolveu as 148 arestas em 20,356 ms.
- `in_degree` conta pares distintos `source_path → destination_path`, portanto chamadas repetidas não inflam a prioridade.
- A UI expõe contagens `resolved`, `ambiguous`, `external`, `unresolved` e a versão do resolvedor.

## Controle BF5 preservado

O controle de tarefa on-demand foi repetido com cinco pares válidos e um warmup por modo, ordem contrabalanceada, hashes equivalentes e provider loopback.

| Medida | Vault vazio | Brownfield mapeado |
|---|---:|---:|
| Qualidade determinística | 5/5 | 5/5 |
| Perguntas | 0 | 0 |
| Tokens de entrada P50 | 5.187 | 5.187 |
| Latência Specification P50 | 755,737 ms | 774,404 ms |

RS1 deliberadamente ainda não entrega o grafo ao contexto da tarefa. Assim, igualdade de qualidade/tokens é o controle correto; a diferença de latência não autoriza rollout. A ativação permanece `keep_opt_in_disabled_by_default`. O benefício entre arquivos será medido separadamente no gate RS2.

## Validação

| Gate | Resultado |
|---|---|
| benchmark RS1 release, 5 pares + warmup | aprovado; relatório JSON persistido |
| golden fixtures e casos negativos | aprovado; precisão/recall 100%, zero falso `resolved` |
| `cargo fmt --all -- --check` | aprovado |
| `cargo test --workspace --all-targets --no-fail-fast` | 368 aprovados, 0 falhas |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | aprovado |
| `cargo build --workspace --release` | aprovado |
| upgrades seriais até V17 | 5 aprovados, 0 falhas |

O warning de compatibilidade futura de `proc-macro-error2 v2.0.1` é preexistente em dependência de terceiros e não é warning de Clippy nem falha do gate.

## Decisão do gate

**RS1-G aprovado.** O grafo persistido é preciso, fail-closed, incremental, observável e respeita os limites de tempo, consentimento e prioridade da tarefa principal. A ativação geral continua não autorizada; o próximo gate é RS2, que consumirá somente relações completas e atuais, com revalidação pelo `RepositoryContext` e orçamento existente.
