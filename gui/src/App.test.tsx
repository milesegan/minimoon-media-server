import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, test } from "vitest";
import type { Status } from "./backend/types";
import { useStore } from "./store";
import { device, folder, running } from "./test/backend";
import { renderApp } from "./test/render";

const starting: Status = {
  phase: "starting",
  error: null,
  dataDir: "/data",
  server: null,
};
const failed: Status = {
  phase: "failed",
  error: "Port 8787 is already in use",
  dataDir: "/data",
  server: null,
};

const section = (name: string) => screen.queryByRole("region", { name });

describe("before the server runs", () => {
  test("starting shows the app's name and no settings", async () => {
    await renderApp(starting);
    expect(
      screen.getByRole("heading", { name: "MiniMoon Media Server" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Starting…")).toBeInTheDocument();
    expect(section("Shared folders")).toBeNull();
    expect(screen.queryByRole("button", { name: /Sharing/ })).toBeNull();
    expect(screen.queryByText("Rescan all folders")).toBeNull();
  });

  test("a failed start says why, and the data folder can still be opened", async () => {
    const { backend, user } = await renderApp(failed);
    expect(screen.getByText("The server couldn't start.")).toBeInTheDocument();
    expect(screen.getByText("Port 8787 is already in use")).toBeInTheDocument();
    expect(screen.getByText("Not running")).toBeInTheDocument();
    expect(section("Devices")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Show data folder" }));
    expect(backend.calls).toEqual([{ command: "show_data_folder", args: {} }]);
  });

  test("a data folder that won't open shows the error", async () => {
    const { user } = await renderApp(failed, {
      show_data_folder: () => Promise.reject("No file manager"),
    });
    await user.click(screen.getByRole("button", { name: "Show data folder" }));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "No file manager",
    );
  });
});

describe("while running", () => {
  test("shows the server's name, every section and the window title", async () => {
    await renderApp(running({ name: "Den" }));
    expect(screen.getByRole("heading", { name: "Den" })).toBeInTheDocument();
    expect(screen.getByText("Sharing on this network")).toBeInTheDocument();
    for (const name of ["Shared folders", "Devices", "This server"]) {
      expect(section(name)).toBeInTheDocument();
    }
    expect(document.title).toBe("Den – MiniMoon Media Server");
  });

  test("the figures show the address, what's playing and what's shared", async () => {
    await renderApp(
      running({
        downloads: 2,
        folders: [
          folder({ id: "a", files: 1000 }),
          folder({ id: "b", files: 234 }),
          folder({ id: "c", files: 99, shared: false }),
        ],
      }),
    );
    expect(screen.getByText("192.168.1.24:8787")).toBeInTheDocument();
    expect(screen.getByText("2 streams")).toBeInTheDocument();
    expect(screen.getByText("1,234 files")).toBeInTheDocument();
  });

  test("clicking the address copies the whole URL", async () => {
    const { user } = await renderApp(running());
    await user.click(screen.getByRole("button", { name: "192.168.1.24:8787" }));
    expect(await navigator.clipboard.readText()).toBe(
      "http://192.168.1.24:8787",
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Copied http://192.168.1.24:8787",
    );
  });

  test("Pause Sharing pauses", async () => {
    const { backend, user } = await renderApp(running());
    await user.click(screen.getByRole("button", { name: "Pause Sharing" }));
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "set_paused", args: { paused: true } },
      ]),
    );
  });

  test("paused, it says so and offers to resume", async () => {
    const { backend, user } = await renderApp(
      running({ paused: true, address: null, downloads: 3 }),
    );
    expect(screen.getByText("Sharing is paused.")).toBeInTheDocument();
    expect(screen.getByText("Paused")).toBeInTheDocument();
    expect(screen.getByText("Off while paused")).toBeInTheDocument();
    // Nothing can play while paused, whatever the count says.
    expect(screen.getByText("Nothing")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Resume Sharing" }));
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "set_paused", args: { paused: false } },
      ]),
    );
  });

  test("controls are disabled while a command is in flight", async () => {
    let finish!: () => void;
    const { user } = await renderApp(running(), {
      set_paused: () => new Promise<void>((resolve) => (finish = resolve)),
    });
    const pause = screen.getByRole("button", { name: "Pause Sharing" });
    await user.click(pause);
    expect(pause).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Rescan all folders" }),
    ).toBeDisabled();
    finish();
    await waitFor(() => expect(pause).toBeEnabled());
  });
});

describe("footer", () => {
  test("Rescan all folders rescans every folder", async () => {
    const { backend, user } = await renderApp(running());
    await user.click(
      screen.getByRole("button", { name: "Rescan all folders" }),
    );
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "rescan", args: { id: null } },
      ]),
    );
  });

  test("Reindex everything confirms it started", async () => {
    const { backend, user } = await renderApp(running());
    await user.click(
      screen.getByRole("button", { name: "Reindex everything" }),
    );
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Reindexing in the background",
    );
    expect(backend.calls).toEqual([
      { command: "reindex_everything", args: {} },
    ]);
  });

  test("a reindex that fails shows only the error", async () => {
    const { user } = await renderApp(running(), {
      reindex_everything: () => Promise.reject("Library is locked"),
    });
    await user.click(
      screen.getByRole("button", { name: "Reindex everything" }),
    );
    await waitFor(() =>
      expect(useStore.getState().toast).toEqual({
        message: "Library is locked",
        error: true,
      }),
    );
  });
});

describe("devices", () => {
  test("lists paired devices, and Revoke asks the backend to revoke one", async () => {
    const { backend, user } = await renderApp(
      running({
        devices: [
          device({
            clientId: "tv-1",
            clientName: "Living Room",
            platform: "tvOS",
          }),
        ],
      }),
    );
    const list = screen.getByRole("list", { name: "Paired devices" });
    expect(within(list).getByText("Living Room")).toBeInTheDocument();
    expect(
      within(list).getByText("MiniMoon on tvOS · Paired today"),
    ).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Revoke Living Room…" }),
    );
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "revoke_device", args: { clientId: "tv-1" } },
      ]),
    );
  });

  test("with pairing on and nothing paired, says where codes will appear", async () => {
    await renderApp(running({ pairing: true, devices: [] }));
    expect(screen.getByText(/No devices paired yet/)).toBeInTheDocument();
  });

  test("the pairing switch turns pairing codes off", async () => {
    const { backend, user } = await renderApp(running({ pairing: true }));
    await user.click(
      screen.getByRole("switch", { name: "Require a pairing code" }),
    );
    await waitFor(() =>
      expect(backend.calls).toEqual([
        { command: "set_pairing", args: { on: false } },
      ]),
    );
  });
});
