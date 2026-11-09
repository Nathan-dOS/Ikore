# Etapa 7 — corte definitivo para GPUI

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

**Data:** 2026-08-26  
**Plataforma do primeiro corte:** Windows x64  
**Estado:** aplicação exclusivamente GPUI/Rust; distribuição externa ainda bloqueada

## Resultado executivo

- `hvc-app` é a única composition root e o único binário desktop.
- Tauri, React, TypeScript, Vite, Monaco, Zustand, Node e IPC foram removidos.
- O workspace contém somente `hvc-app`, `hvc-core`, `hvc-editor`, `hvc-ui` e
  `hvc-test-support`.
- O teste de integração e o ícone Windows foram transferidos antes da exclusão.
- O `Cargo.lock` não contém Tauri, Wry ou WebKit.
- O ledger histórico preserva a rastreabilidade dos 51 testes frontend para os
  51 cenários Rust equivalentes.
- O código removido é recuperável pelo checkpoint `6a06945`, criado antes do corte.

## Otimizações avaliadas

| Identificador | Mudança | Decisão | Resultado |
|---|---|---|---|
| HVC-GPUI-001 | Reutilizar o dispositivo D3D11 criado ao validar o adaptador | Aceita | teste isolado preliminar reduziu 615,254 ms para 534,180 ms, ganho de 13,177% |
| HVC-GPUI-002 | Desabilitar DirectComposition | Rejeitada | diferença aproximada de 0,334%, sem benefício material |
| HVC-APP-001 | Inicializar `AppCore` em paralelo a `Application::new()` | Aceita | elimina a soma serial de migrations/recovery com a inicialização gráfica; o core só é entregue pronto à UI |
| HVC-APP-002 | Carregar Settings e Logs somente ao abrir/restaurar essas views | Aceita | remove consultas não essenciais da abertura e preserva restauração e ressincronização |
| HVC-APP-003 | Compartilhar o cliente HTTP/TLS entre persistência/LLM e Chat | Aceita | elimina a criação de um segundo cliente no startup sem alterar recovery ou streaming |
| HVC-APP-004 | Tornar todas as entidades e inputs secundários opcionais | Adiada | o custo observado do conjunto é pequeno e a mudança ampliaria risco em foco, IME e restauração das seis views |
| HVC-GPUI-003 | Lazy init de DirectWrite, shaders e pipelines internos | Adiada | alteração extensa do backend gráfico, sem segurança suficiente para o corte; requer matriz própria de GPUs/drivers |
| HVC-GPUI-004 | Chave de cache da medição de texto inclui a largura de truncamento (2026-09-24) | Aceita | corrige a reticência de texto em linha única, que nunca aparecia; texto sem `text_overflow` mantém o comportamento anterior |
| HVC-GPUI-005 | Foco por teclado visível: `Window::focus_visible`, estilo `focus_visible` e tooltip aberto no foco por Tab (2026-09-25) | Aceita | permite navegar pelo Tab com anel de foco só pelo teclado e ler as dicas "i" sem mouse; `.focus()` mantém o comportamento anterior |

O GPUI vendorizado e o patch efetivamente adotado estão documentados em
[`../third_party/gpui/HVC-PATCHES.md`](../third_party/gpui/HVC-PATCHES.md). Não
foram mantidas mudanças rejeitadas no código de produção.

## Aferição curta do corte

O comparativo pré-corte repetiu dez execuções por implementação usando o mesmo
marco da Etapa 6: janela Win32 visível e SQLite criado.

| Métrica P50 | GPUI otimizado | Tauri legado | Redução |
|---|---:|---:|---:|
| Abertura | 247,498 ms | 528,724 ms | 53,190% |
| Memória ociosa | 140,422 MiB | 361,254 MiB | 61,129% |

A primeira execução GPUI chegou a 802,638 ms, evidenciando a influência de
cache, GPU/driver e warmup do sistema. Por isso o resultado demonstra que a meta
de 40% foi atingida nessa rodada e máquina, mas não constitui garantia universal.
O relatório bruto está em
[`reports/stage7-pre-cut-comparison-2026-08-26.json`](reports/stage7-pre-cut-comparison-2026-08-26.json).

## Gate técnico

Foram aprovados:

- estrutura exclusivamente GPUI/Rust;
- `cargo fmt --all -- --check`;
- Clippy do workspace com `-D warnings`;
- 186 testes Rust, sendo 185 unitários e 1 integração;
- build release de `hard-vibe-code.exe`;
- smoke Windows com banco isolado, exit code 0 e shutdown gracioso;
- auditoria de 549 pacotes Rust sem licença negada, metadado ausente ou fonte não permitida;
- RustSec com zero vulnerabilidades.

Permanece somente o aviso de incompatibilidade futura de
`proc-macro-error2 2.0.1`, transitivo e já registrado.

## Windows activation follow-up — 2026-09-09

HVC-GPUI-002 records the subsequent `afac996` startup/activation correction in `third_party/gpui/HVC-PATCHES.md`. Source review confirms normal-window visibility, hidden-window activation and the separate minimized-window restore path. A fresh physical Windows smoke for this patch is deferred; the historical Stage 7 results above do not certify this later change. Current synchronization evidence is in `quality/cerebro-sync-validation-2026-09-09.md`.

## Persistent message text follow-up — 2026-11-08

HVC-GPUI-006 corrects text measurement reuse when a definite wrapping width
becomes unconstrained. Its new `hvc-ui::selectable_message::tests` regression
failed against the earlier condition. Companion message pieces now retain
unchanged unselected text layouts through a local wrapper; the complete
inherited text style and rem size invalidate them, and selected pieces rebuild
the consumed highlight recipe. Full Markdown parsing, content, workload and
speed thresholds are unchanged.

Focused validation: `cargo test --locked -p hvc-ui --lib
selectable_message::tests -- --test-threads=2` passed all 10 selected tests
(five added), with zero failed/ignored and 470 filtered. Before the vendor
condition changed, the narrow-to-intrinsic regression failed; the first
focused run also exposed and corrected two test-fixture issues (a rem change
needed an explicit redraw, and the diff-copy control needed a debug selector).
The confirmed rerun is in `target/companion-final/text-layout-tests.log`.

The following allocation-only refinement borrows the cached Markdown AST and
text in the Companion renderer, allocating owned text only for changed cache
pieces and reserving message-buffer capacity. Existing Workspace/Vault/Notes/
Review consumers retain their original owned-string path. Three new tests
verify borrowed storage, the owned path's moved storage, and matching geometry
across all supported block types. The focused suite then passed 13/13, zero
failed/ignored and 470 filtered; the log is
`target/companion-final/borrowed-markdown-tests.log`. Its release speed result
is not established by these functional tests.

Disposition: **deferred for integration** pending the release speed comparison
and complete delivery gate. The earlier Stage 7 results above do not certify
this follow-up.

## Bounded text measurement follow-up — 2026-11-08

HVC-GPUI-007 replaces the single text measurement with a current state plus
three LRU states, keyed by exact wrapping/truncation constraints. A debug-only
trace established repeated constraint misses; the instrumented sources were
then restored byte for byte to all 965 qualified source hashes before this
implementation began. No diagnostic instrumentation remains in production.

Test-support-only helpers were added first to the single-state implementation.
The new behavioral tests reproduced four-constraint measurements growing from
4 to 16, truncation measurements growing from 3 to 12, and a missed LRU revisit;
the 13 existing message tests still passed. Logs:
`target/companion-final/lru-red-tests-complete.log`.

The focused green rerun passed 16/16 after formatting, with zero failed/ignored
and 470 filtered. The three new cases prove retained measurements stop growing
after warming their exact constraints, enforce the four-state/LRU bound and
preserve truncation/Unicode geometry. Existing style/rem, selection and complete
table/diff-copy scenarios also passed. Log:
`target/companion-final/lru-green-tests.log`.

Disposition: **deferred for integration**, pending strict checks, the complete
regression gate and unchanged-workload release speed comparison. Debug trace
timings and focused functional tests do not establish that gate.

## No-color text background follow-up — 2026-11-09

HVC-GPUI-008 skips unused font-metric queries and glyph traversal only when all
decoration backgrounds are `None`. The guard remains inside `paint_layer`,
preserving its bounds, empty layer operations and draw-order/replay behavior.
Any `Some` background, including transparent colors, keeps the original path;
foreground text, underline and strikethrough are unaffected. No workload,
readiness, warmup or latency limit changed.

Six private GPUI tests were written first. The RED run passed five and failed
only the no-color font-metric assertion. The GREEN run passed all six, zero
failed/ignored and 70 filtered. The same real paint tests cover colored and
transparent quads, mixed ranges, wrapped Unicode, foreground decorations,
overlapping primitive order, exact Scene replay and a balanced layer stack.
Logs are `target/companion-final/no-background-2026-11-09/red-tests-resolved.log`
and `green-private-tests.log`.

The normal `-p gpui` selection cannot execute a non-member's development tests.
A temporary harness therefore adds GPUI membership, omits the unused
`env_logger`, `pretty_assertions` and Lyon-extra development blocks, and resolves
offline only missing dependency edges. Programmatic comparison preserves all
794 package identities (name, version, source and checksum); no package was
added, removed or upgraded. Precise diff, no-use search, metadata and proof are
archived beside the logs. Initial selection/DNS/resolution failures were setup
failures, not the behavioral RED result.

Exact selected command:

```powershell
cargo test --offline --locked -p gpui --lib --no-default-features --features test-support,windows-manifest text_system::line::tests -- --test-threads=2
```

These six vendor tests are outside the default workspace-suite count. The
temporary harness was retained for the following foreground tests, then
restored byte for byte before handoff. No release build,
complete regression gate or speed qualification is claimed for HVC-GPUI-008.
Disposition: **deferred for integration** pending those checks.

## Foreground vertical visibility follow-up — 2026-11-09

HVC-GPUI-009 keeps the foreground paint layer and permits an early return only
when every underline/strikethrough is `None` and the old vertical glyph-guard
predicate cannot intersect the content mask. The envelope repeats Y additions
for every wrap and includes the maximum height of the same font bounds.
Invalid/non-finite values and every `Some` decoration fall through unchanged.
No simple line-bounds rejection, horizontal culling, cadence or fixture change
was introduced.

Stage 1 applied only a private test counter and four behavioral tests. The RED
selection executed ten cases: seven passed and three failed with one or two
unnecessary glyph visits where zero were expected. The `Some` fallback and all
six background cases passed. Stage 2 added the guard and two property/fallback
cases. The GREEN selection passed 12/12, zero failed/ignored and 70 filtered.
It covers overhang/subpixel boundaries, partially visible emoji/wrapped rows,
multiple font heights, decoration fallback, invalid inputs and exact Scene
order/replay. The counter is `cfg(test)` only and creates no release API/state.

The complete vendor lib inventory was then listed: 82 tests. The unfiltered run
passed all 82 with zero failed, ignored or filtered. It includes the 12 focused
cases, so these counts are not additive and remain separate from `test-all`.
Exact complete command, after listing with `-- --list`:

```powershell
cargo test --offline --locked -p gpui --lib --no-default-features --features test-support,windows-manifest -- --test-threads=2
```

Logs, executed source and qualification data are in
`target/companion-final/vertical-culling-2026-11-09/`. The temporary harness
retained the same 794 package identities; after the tests, the root manifest,
vendor manifest and lock were restored to their byte-exact backups. No release
build, full application gate or speed acceptance is claimed for this source.
Disposition: **deferred for integration** pending those checks.

## Distribuição

O executável release está tecnicamente operável, mas distribuição externa não
está autorizada. A política mantém o bloqueio jurídico associado ao issue Zed
#55470 até resolução upstream ou decisão jurídica explícita. Instalador e
assinatura de código também permanecem para a etapa final.
