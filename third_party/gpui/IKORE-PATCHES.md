# GPUI vendorizado

Esta pasta contém o pacote `gpui` 0.2.2 publicado no crates.io, sob a licença
Apache-2.0 preservada em `LICENSE-APACHE`.

O código é mantido localmente porque o primeiro corte do Ikore (anteriormente
Hard Vibe Code) é Windows x64 e precisa de um backport pequeno e auditável do backend oficial.
Não há componentes de editor ou UI do Zed nesta cópia.

Os identificadores dos patches mantêm o prefixo histórico `HVC-GPUI-00N` (Recomendação R3),
garantindo rastreabilidade e consistência com os relatórios de auditoria e ADRs.

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

## HVC-GPUI-002 — Windows window visibility and activation

Source: HVC commit `ee898342bfb6df157035ec2c7d517649708a24b0` (2026-09-04), a local integration correction; no upstream backport provenance is claimed.

`src/platform/windows/window.rs` sets `placement.showCmd` to `SW_SHOWNORMAL`, calls `ShowWindowAsync(SW_SHOWNORMAL)` after applying visible window placement, and shows a hidden window during activation. Minimized windows retain the `SW_RESTORE` path. The application shell explicitly calls `activate_window()` on the newly opened window before `cx.activate(true)`.

Validation disposition: implementation verified by source review on 2026-09-09; fresh physical Windows startup/activation smoke remains deferred. See `quality/stage7-report.md` and `quality/cerebro-sync-validation-2026-09-09.md`.

## HVC-GPUI-004 — ellipsis on single-line text

Arquivo: `src/elements/text.rs`

Local correction (2026-09-24); no upstream backport provenance is claimed.

Text styled `whitespace_nowrap().text_ellipsis()` is measured first without a
definite width (no truncation). The measurement cache compared only
`wrap_width`, which is `None` for single-line text, so the later measurement
with the real width returned the untruncated size: the ellipsis never appeared
and the text overflowed or was clipped (explorer file names, Forge graph nodes,
select values). `TextLayoutInner` now records `truncate_width` and the cache
hit also requires it to match. Text without `text_overflow` keeps
`truncate_width = None` and the previous behaviour.

Validation: `cargo test -p hvc-ui --lib` and a physical smoke on Windows 11
(isolated profile) where the Workspace tree shows `contract-templa…` at
1280×800 instead of a clipped name. See the 2026-09-24 "responsiveness and
workspace/agents/vault/forge polish" report in the maintainers' design archive; see the public [historical evidence context](../../quality/README.md#historical-design-references).

Follow-up (2026-09-25): with truncation now recomputed per width, the measure
closure truncated the element's captured `runs` in place, and Taffy calls it
several times per layout. A later, wider truncation then shaped text whose runs
still ended where the earlier cut left them; when that end fell inside the
ellipsis, DirectWrite panicked (`end byte index 73 is not a char boundary; it is
inside '…'`, `src/platform/windows/direct_write.rs`). `LineWrapper` gains
`truncate_for_layout`, which truncates a copy of the runs per measurement, and
`src/elements/text.rs` uses it; a `debug_assert` checks that the runs cover the
shaped text. Tests: `repeated_measurements_truncate_from_the_original_runs`
(`line_wrapper.rs`) and, in `hvc-ui`,
`truncating_a_label_never_leaves_its_runs_cut_for_another_width`; with the old
in-place code the assertion failed in dozens of existing `hvc-ui` layout tests.

## HVC-GPUI-005 — keyboard focus visibility and focus tooltips

Files: `src/window.rs`, `src/elements/div.rs`

Local addition (2026-09-25); no upstream backport provenance is claimed.

`Window` records whether the current focus was reached with the keyboard:
`focus_next` / `focus_prev` set `focus_visible`; `focus` (programmatic or by
click), `blur` and any mouse press clear it; `Window::focus_visible()` reads it.
Elements gain a `focus_visible(|style| ...)` refinement applied only while they
are focused in that state (`focus(...)` keeps its meaning). During an element's
prepaint, an element with a tooltip that is focused and keyboard-visible shows
the tooltip anchored under its bottom-left corner, kept while it stays focused
and cleared when focus moves or the mouse is pressed. The tooltip is set in
prepaint because `Window::refresh` is ignored while drawing.

Validation: `cargo test -p hvc-ui --lib`
(`keyboard_focus_presses_once_and_opens_tooltips_under_the_control`,
`tab_moves_focus_through_controls_and_stays_inside_settings`) and a physical
smoke on Windows 11 (isolated profile). See the 2026-09-25 "Home dashboard,
keyboard navigation and icon controls" report in the maintainers' design archive; see the public [historical evidence context](../../quality/README.md#historical-design-references).

## HVC-GPUI-006 — exact wrapping constraints for text measurement reuse

File: `src/elements/text.rs`

Local correction (2026-11-08); no upstream backport provenance is claimed.

The text measurement cache formerly reused wrapped lines whenever a new
measurement had no definite wrapping width. A persistent text element measured
at a narrow width and then with intrinsic (`MaxContent`) constraints therefore
kept the narrow size and wrapped lines. Cache reuse now requires exact equality
of `wrap_width`, including `None`, alongside the existing truncation key.

The application retains unchanged unselected message pieces through a local
`StyledText` wrapper, invalidating the full inherited text style and rem size.
Selected pieces remain fresh because layout consumes their highlight recipe.
The complete Markdown source and clipboard text are preserved.

Regression: `cached_text_remeasures_when_wrapping_becomes_unconstrained` in
`hvc-ui::selectable_message::tests` failed with the previous condition. The same
suite covers inherited style/rem changes, resizing and complete Unicode,
table and diff copy. Current test results and qualification disposition are
recorded in [`quality/stage7-report.md`](../../quality/stage7-report.md);
speed and complete-gate qualification are tracked separately.

## HVC-GPUI-007 — bounded text measurement states

File: `src/elements/text.rs`

Local correction (2026-11-08); no upstream backport provenance is claimed.

A temporary debug trace of the unchanged three-stream workload observed 12
measurement requests per message piece and six cache misses. Almost every
miss changed wrapping constraints on an existing piece. A single active
measurement therefore discarded useful intrinsic and constrained states.
The trace establishes repeated work; its debug timings do not qualify release
performance.

`TextLayout` now retains at most four complete measurement states: the current
state and three lazily allocated LRU entries. Lookup uses exact wrapping and
truncation widths, including `None`, preserving HVC-GPUI-004 and HVC-GPUI-006.
Promotion moves boxed states without cloning lines or inline decorations.
Activating or saving a state clears its old bounds; prepaint always installs
the current position. Text and style identity remain the owner's responsibility:
Companion replaces the entire layout when text, inherited style, rem size or
selection styling changes.

Read-only `measurement_count` and `retained_measurement_count` helpers and the
counter exist only under `test`/`test-support`, with no release field or API.
The GPUI-consuming UI tests assert repeated-constraint cache hits, four-state
capacity, LRU eviction, intrinsic geometry, Unicode hit testing, truncation and
complete copy/selection behavior. They failed against the single-state cache
with actual repeated measurements before the implementation changed. Current
results and remaining qualification are recorded in
[`quality/stage7-report.md`](../../quality/stage7-report.md).

## HVC-GPUI-008 — skip background work when every decoration has no color

File: `src/text_system/line.rs`

Local correction (2026-11-09); no upstream backport provenance is claimed.

`paint_line_background` returns successfully inside the existing `paint_layer`
closure when every decoration run has `background_color: None`, including an
empty run slice. The layer still records the same bounds, draw ordering and
replay operations. Only the unused font-metric queries and glyph walk are
skipped. Any `Some` background, including a transparent color, follows the
unchanged painting path. Foreground, underline and strikethrough are unchanged.

Six private vendor tests exercise the actual paint path and Scene. The new
no-color test failed first because the synthetic font's metrics were loaded;
the same selection passed after the guard. Coverage includes mixed/transparent
backgrounds, wrapped Unicode, foreground decorations, overlapping order, exact
Scene replay and a balanced layer stack. No public test-only API was added.

The vendor is not a normal workspace member. The tests use the temporary,
offline/locked harness documented in the validation record; default workspace
tests do not run these six private tests. Focused success does not qualify
release speed or the complete delivery gate. See
[`quality/stage7-report.md`](../../quality/stage7-report.md).

## HVC-GPUI-009 — conservative foreground vertical visibility guard

File: `src/text_system/line.rs`

Local correction (2026-11-09); no upstream backport provenance is claimed.

Inside the existing foreground paint layer, the guard skips glyph traversal
only when every underline/strikethrough is `None` and no glyph can satisfy the
existing vertical `max_glyph_bounds.intersects(mask)` predicate. It repeats
the original per-wrap Y additions, bounds every row and uses the largest
finite non-negative height of the same per-font bounding boxes. Invalid or
non-finite inputs retain the original path. Every `Some` decoration, including
transparent, zero-thickness and zero-length runs, also retains that path.
This bounds the old predicate; it does not substitute the simpler line bounds
or claim to model physical raster bounds. Layer/order/replay operations remain.

A private `cfg(test)` glyph-visit counter first demonstrated three behavioral
failures (7 passed/3 failed) before the guard. The combined foreground/background
selection then passed 12/12, covering overhang, subpixel edge contact, partially
visible wrapped emoji, multiple font heights, decoration fallback, invalid
inputs, exact Scene replay/order and repeated-addition enumeration. All 82
non-ignored vendor library tests subsequently passed, with zero failures,
ignored or filtered. The counter has no release state or public API.

The tests used the same temporary offline/locked harness as HVC-GPUI-008;
its three manifest/lock files were restored byte for byte afterwards. No
workload, warmup, readiness or latency limit changed. Release speed and the
complete application delivery gate remain pending. See
[`quality/stage7-report.md`](../../quality/stage7-report.md).

## HVC-GPUI-010 — test-only DPI changes and real layout-node observation

Files: `src/platform/test/window.rs`, `src/app/test_context.rs`,
`src/taffy.rs` and `src/window.rs`.

Local test infrastructure (2026-11-09); no upstream backport is claimed.
The test platform retains its default scale of 2.0 and can simulate a finite,
positive DPI change through the existing resize callback, preserving logical
viewport size. Invalid values are rejected before state mutation. Test context
helpers expose this operation to UI tests so fractional DPI exercises the
real Window/Taffy scale rather than a rem-size approximation.

A read-only node-count getter exists only under `test`/`test-support`. It
observes the real Taffy tree without counters or layout retention, returns
`None` while measurement temporarily takes the engine, and zero after a
completed frame clears the tree. All three private seam tests passed. The
complete vendor library passed 85/85 with zero failed/ignored/filtered; the
three are included. The temporary offline/locked harness preserved all 796
package identities and restored both manifests and the lock byte for byte.
The application paragraph tests use it to prove one redundant node is removed
without changing native measurement or selection. This adds no release state,
benchmark override, workload change or latency-limit exception.

Execution and remaining qualification are recorded in
[`quality/stage7-report.md`](../../quality/stage7-report.md) and the Companion
validation record. The private tests must run explicitly; workspace tests do
not execute a dependency's test module.

## Integridade

- Versão-base: `gpui` 0.2.2.
- Fonte-base: crates.io.
- Functional patches are maintained in `src/platform/windows/directx_devices.rs` (HVC-GPUI-001), `src/platform/windows/window.rs` (HVC-GPUI-002) and `src/elements/text.rs` (HVC-GPUI-004, HVC-GPUI-006, HVC-GPUI-007), plus `src/window.rs` and `src/elements/div.rs` (HVC-GPUI-005) and `src/text_system/line.rs` (HVC-GPUI-008, HVC-GPUI-009).
- Ajuste adicional em `src/taffy.rs` explicita literais `f32` exigidos pelo
  Rust 1.98; ele não altera comportamento nem desempenho.
- Mudanças futuras devem receber um identificador `HVC-GPUI-NNN` e registrar
  resultado aceito, rejeitado ou adiado em `quality/stage7-report.md`.
