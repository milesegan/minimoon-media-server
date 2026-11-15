// Runs before every test file (see `test.setupFiles` in vite.config.ts).

import "@testing-library/jest-dom/vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";
import { useStore } from "../store";

afterEach(async () => {
  vi.useRealTimers();
  cleanup();
  // Unmounting stops event listeners asynchronously; let that finish before the mocks go.
  await new Promise((resolve) => setTimeout(resolve));
  clearMocks();
  vi.restoreAllMocks();
  // Each test starts from the store's initial state, as a fresh window would.
  useStore.setState(useStore.getInitialState(), true);
});
