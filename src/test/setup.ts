import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

import { resetTauriMock } from "./mocks/tauriMock";

vi.mock("@tauri-apps/api/core", async () => {
  const { tauriInvokeMock } = await import("./mocks/tauriMock");
  return { invoke: tauriInvokeMock };
});

vi.mock("@tauri-apps/plugin-dialog", async () => {
  const { tauriOpenDialogMock } = await import("./mocks/tauriMock");
  return { open: tauriOpenDialogMock };
});

afterEach(() => {
  cleanup();
  resetTauriMock();
});
