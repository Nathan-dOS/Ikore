# GPUI vendorizado

Esta pasta contém o pacote `gpui` 0.2.2 publicado no crates.io, sob a licença
Apache-2.0 preservada em `LICENSE-APACHE`.

O código é mantido localmente porque o primeiro corte do Hard Vibe Code é
Windows x64 e precisa de um backport pequeno e auditável do backend oficial.
Não há componentes de editor ou UI do Zed nesta cópia.

## HVC-GPUI-001 — reutilização do dispositivo D3D11

Arquivo: `src/platform/windows/directx_devices.rs`

O GPUI 0.2.2 criava um dispositivo D3D11 para validar cada adaptador, descartava
o primeiro dispositivo válido e criava outro logo depois. O backport retorna o
adaptador, o dispositivo, o contexto e o feature level produzidos pela própria
validação.

Origem da correção: implementação posterior do backend Windows oficial do GPUI.

Resultado preliminar em 26/08/2026, no mesmo computador e com diretório de dados
novo: aproximadamente 615,254 ms para 534,180 ms até a janela Win32, uma melhora
de 13,177%. O gate funcional e o smoke test Windows continuam obrigatórios.

## Integridade

- Versão-base: `gpui` 0.2.2.
- Fonte-base: crates.io.
- Patch funcional mantido exclusivamente no arquivo indicado acima.
- Ajuste adicional em `src/taffy.rs` explicita literais `f32` exigidos pelo
  Rust 1.98; ele não altera comportamento nem desempenho.
- Mudanças futuras devem receber um identificador `HVC-GPUI-NNN` e registrar
  resultado aceito, rejeitado ou adiado em `quality/stage7-report.md`.
