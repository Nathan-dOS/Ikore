# Etapa 6 — hardening, benchmarks e auditorias

**Data da execução:** 2026-08-26  
**Plataforma:** Windows x64, 8 processadores lógicos, display de 60 Hz  
**Estado:** implementação concluída; gate final reprovado e distribuição bloqueada

## Resultado executivo

| Gate | Resultado | Evidência |
|---|---:|---|
| Gate técnico consolidado | Aprovado | formatação, Clippy `-D warnings`, 186 testes Rust, 51 frontend, dois builds release e smoke GPUI |
| Shutdown de jobs/LLM/Chat | Aprovado | cancelamento em duas fases, join limitado e testes Rust |
| Licenças e fontes Rust/npm | Aprovado tecnicamente | nenhum GPL/AGPL/SSPL, licença ausente ou fonte fora dos registros permitidos |
| Advisories Rust/npm | Aprovado para vulnerabilidades | 0 vulnerabilidades RustSec e 0 npm |
| Memória ociosa GPUI vs Tauri | Aprovado | redução P50 de 60,831% |
| Cold start GPUI vs Tauri | Reprovado | redução P50 de 14,566%; meta mínima de 40% |
| Frame cadence GPUI | Reprovado | P95 18,2096 ms e P99 19,4284 ms; metas 8,33/16,67 ms |
| Distribuição externa | Bloqueado | issue Zed #55470 aberto e sem decisão jurídica explícita |

A Etapa 7 não pode começar enquanto os gates reprovados não forem corrigidos ou formalmente redefinidos nos ADRs. Tauri/React permanece funcional e não foi removido.

## Hardening aplicado

- O shutdown bloqueia novos claims do worker, cancela todos os tokens de jobs/LLM ativos e mantém o worker vivo até persistir o estado terminal ou atingir o timeout.
- O Chat cancela todos os turnos e `wait_for_idle` comprova que o registro de execuções ficou vazio antes de encerrar o runtime.
- O adaptador Tauri passou a respeitar `HVC_DATA_DIR`, permitindo banco isolado e o mesmo contrato de exclusão mútua usado nos testes/benchmarks.
- O benchmark de frame usa `Window::request_animation_frame`; estados `configured`, `measuring`, `completed` e `failed` impedem timeout silencioso.

## Metodologia dos benchmarks

O comparativo executa cada implementação dez vezes, alternando a ordem Tauri→GPUI e GPUI→Tauri. Cada execução recebe uma cópia exclusiva do mesmo dataset vazio controlado. “Pronto” significa janela Win32 visível e SQLite criado. A memória é o working set de toda a árvore de processos após dois segundos ociosa; portanto, subprocessos WebView2 entram na conta do Tauri.

| Métrica P50 | GPUI | Tauri | Diferença |
|---|---:|---:|---:|
| Startup | 427,464 ms | 500,344 ms | 14,566% menor |
| Memória ociosa | 141,262 MiB | 360,645 MiB | 60,831% menor |

O executável GPUI medido tem 14.976.512 bytes; o executável Tauri tem 12.204.544 bytes. Esses números são de executáveis release, não de instaladores, e o tamanho do Tauri não inclui o runtime WebView2 já instalado no sistema.

O probe GPUI coletou 120 intervalos após 20 frames de warmup: P95 de 18,2096 ms e P99 de 19,4284 ms. A máquina reportou display de 60 Hz, cujo período nominal é 16,67 ms. Isso explica por que a meta de 8,33 ms não é demonstrável neste display, mas não altera o resultado formal: o gate aprovado continua reprovado e o P99 também excedeu 16,67 ms.

Evidências versionadas:

- [`reports/stage6-windows-comparison-2026-08-26.json`](reports/stage6-windows-comparison-2026-08-26.json)
- [`reports/stage6-ui-latency-2026-08-26.json`](reports/stage6-ui-latency-2026-08-26.json)

## Supply chain

A auditoria conservadora do grafo Cargo resolvido e do lock npm encontrou 656 pacotes Rust e 412 pacotes npm, sem licenças negadas, metadados ausentes ou fontes externas aos registros permitidos. Cinco crates MPL-2.0 foram registradas como notice, não como GPL.

RustSec encontrou zero vulnerabilidades. O relatório também contém 26 avisos `unmaintained` e três `unsound`; os três `unsound` são `git2`/`glib` presentes no lock multiplataforma e não aparecem no grafo Windows de `hvc-app`. Dependências não mantidas que chegam ao Windows via GPUI continuam registradas como risco de manutenção, embora não façam o scanner retornar falha de vulnerabilidade. O npm audit ficou em zero após atualizar somente o toolchain legado de build/teste.

O grafo Windows fixado em `gpui 0.2.2` não contém `sum_tree`, `ztracing` ou `zlog`. Mesmo assim, a política aprovada mantém a distribuição bloqueada enquanto o issue upstream #55470 permanecer aberto ou até existir decisão jurídica explícita.

Evidências versionadas:

- [`reports/stage6-supply-chain-2026-08-26.json`](reports/stage6-supply-chain-2026-08-26.json)
- [`reports/stage6-advisories-2026-08-26/summary.json`](reports/stage6-advisories-2026-08-26/summary.json)
- [`distribution-policy.json`](distribution-policy.json)

## Como reproduzir

```powershell
# Gate técnico sem repetir rede/benchmarks
powershell -ExecutionPolicy Bypass -File scripts/gate-stage6.ps1 -SkipBenchmarks -SkipAdvisories

# Auditorias
powershell -ExecutionPolicy Bypass -File scripts/audit-supply-chain.ps1
powershell -ExecutionPolicy Bypass -File scripts/audit-advisories.ps1

# Benchmarks (abrem janelas visíveis durante a medição)
powershell -ExecutionPolicy Bypass -File scripts/benchmark-windows.ps1 -Iterations 10
powershell -ExecutionPolicy Bypass -File scripts/benchmark-ui-latency.ps1

# Gate integral; atualmente termina com falha nos thresholds de desempenho
powershell -ExecutionPolicy Bypass -File scripts/gate-stage6.ps1
```

`cargo-audit` foi instalado localmente em `target/tools/cargo-audit`; o script não depende de instalação global nesta máquina. Em um checkout novo, prepare a ferramenta com `cargo install cargo-audit --version 0.22.2 --locked --root target/tools/cargo-audit`. Consultas de advisories e árvore de processos Windows precisam de rede/permissão adequadas.
