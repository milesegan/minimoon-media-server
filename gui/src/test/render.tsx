import { render, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect } from "vitest";
import App from "../App";
import type { Status } from "../backend/types";
import { useStore } from "../store";
import { fakeBackend } from "./backend";

/**
 * Renders the whole window against a fake backend answering with `status`, once the first
 * status has arrived. `handlers` answer other commands, as for `fakeBackend`.
 */
export async function renderApp(
  status: Status,
  handlers?: Parameters<typeof fakeBackend>[1],
) {
  const backend = fakeBackend(status, handlers);
  const user = userEvent.setup();
  const view = render(<App />);
  await waitFor(() => expect(useStore.getState().status).not.toBeNull());
  return { backend, user, ...view };
}
