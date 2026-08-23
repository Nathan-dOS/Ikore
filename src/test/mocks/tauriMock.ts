import { vi } from "vitest";

export const tauriInvokeMock = vi.fn<
  (command: string, args?: Record<string, unknown>) => Promise<unknown>
>();
export const tauriOpenDialogMock = vi.fn<() => Promise<string | string[] | null>>();
export const tauriListenMock = vi.fn<
  (event: string, handler: (event: unknown) => void) => Promise<() => void>
>();

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
  tauriListenMock.mockReset();
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
}
