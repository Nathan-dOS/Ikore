# Etapa 7 — corte definitivo para GPUI

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

## Distribuição

O executável release está tecnicamente operável, mas distribuição externa não
está autorizada. A política mantém o bloqueio jurídico associado ao issue Zed
#55470 até resolução upstream ou decisão jurídica explícita. Instalador e
assinatura de código também permanecem para a etapa final.
