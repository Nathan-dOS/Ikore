# Hard Vibe Code

Desktop Orchestrator nativo em GPUI/Rust para Windows x64.

## Estado atual

- `hvc-app` é o único binário desktop e gera `hard-vibe-code.exe`.
- As seis views — Workspace, Pulsations, Vault, Chat Companion, Settings e Logs — são nativas.
- UI e core compartilham o mesmo processo e usam chamadas Rust tipadas, sem JSON/IPC.
- O banco permanece em `%APPDATA%\com.hardvibecode.desktop\hard-vibe-code.db`.
- `HVC_DATA_DIR` permite isolar dados de desenvolvimento e teste.
- Migrations V1–V9 e o serviço `hard-vibe-code.runtime` no keyring foram preservados.

## Stack

- Rust 1.98.0, edition 2024 e Cargo resolver 3.
- GPUI 0.2.2 com backend Windows e patch local auditável.
- Tokio multi-thread para rede, streaming e workers.
- SQLite via `rusqlite`/`refinery`.
- Rope e `EntityInputHandler` para editor Unicode/IME.
- Tree-sitter Markdown incremental e `pulldown-cmark` para preview sem HTML bruto.
- `notify` para filesystem e `keyring` para credenciais.

Não são reutilizadas crates de editor/UI do Zed. A única fonte vendorizada é o
próprio GPUI 0.2.2, sob Apache-2.0, documentado em
[`third_party/gpui/HVC-PATCHES.md`](third_party/gpui/HVC-PATCHES.md).

## Workspace Cargo

- `crates/hvc-app`: composition root e binário GPUI.
- `crates/hvc-core`: domínio, persistência, workspace, LLM, Chat e barramento tipado.
- `crates/hvc-ui`: shell, design system e as seis views GPUI.
- `crates/hvc-editor`: Rope, seleção, undo/redo, diff, Markdown e Tree-sitter.
- `crates/hvc-test-support`: ambientes isolados, doubles e fixtures de paridade.

## Executar

```powershell
cargo run -p hvc-app
```

Build release e smoke Windows:

```powershell
cargo build --release -p hvc-app
powershell -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1
```

O artefato atual é o executável release. Instalador NSIS/WiX e Authenticode
pertencem à etapa posterior de distribuição.

## Gate da Etapa 7

```powershell
powershell -ExecutionPolicy Bypass -File scripts/gate-stage7.ps1
```

O gate comprova estrutura exclusivamente GPUI/Rust, formatação, Clippy com
`-D warnings`, testes do workspace, build release, smoke Windows e auditorias
Rust de licenças, fontes e advisories.

## Paridade e desempenho

O ledger [`parity/gpui-frontend-ledger.json`](parity/gpui-frontend-ledger.json)
preserva a rastreabilidade dos 51 testes do frontend removido para seus testes
Rust equivalentes.

No comparativo curto do corte, o P50 até janela visível + SQLite foi de
247,498 ms no GPUI e 528,724 ms no Tauri, redução de 53,19%. A memória ociosa
P50 caiu 61,129%. Os números descrevem a máquina e metodologia registradas no
relatório; não são garantias universais.

A distribuição externa permanece bloqueada até a auditoria final e a
resolução ou decisão jurídica explícita sobre o issue Zed #55470.
