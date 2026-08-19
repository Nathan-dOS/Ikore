import { vi } from "vitest";

export const tauriInvokeMock = vi.fn<
  (command: string, args?: Record<string, unknown>) => Promise<unknown>
>();
export const tauriOpenDialogMock = vi.fn<() => Promise<string | string[] | null>>();

export function installTauriRuntime(): void {
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: {
      invoke: tauriInvokeMock
    }
  });
}

export function resetTauriMock(): void {
  tauriInvokeMock.mockReset();
  tauriOpenDialogMock.mockReset();
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
}
