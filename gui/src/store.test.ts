import { mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { Status } from "./backend/types";
import {
  FAILED,
  selectBusy,
  selectReady,
  selectServer,
  useStore,
} from "./store";
import { device, fakeBackend, pairingRequest, running } from "./test/backend";

const store = () => useStore.getState();

/** A promise and the function that settles it, to hold a fake command mid-flight. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => (resolve = settle));
  return { promise, resolve };
}

describe("refresh", () => {
  test("stores the status", async () => {
    fakeBackend(running({ name: "Den" }));
    await store().refresh();
    expect(selectServer(store())?.name).toBe("Den");
  });

  test("turns a failed fetch into a failed status", async () => {
    mockIPC(() => Promise.reject("The window's backend went away"));
    await store().refresh();
    expect(store().status).toEqual({
      phase: "failed",
      error: "The window's backend went away",
      dataDir: "",
      server: null,
    });
  });

  test("doesn't start a second fetch while one is in flight", async () => {
    const fetches: ReturnType<typeof deferred<Status>>[] = [];
    mockIPC(() => {
      const fetch = deferred<Status>();
      fetches.push(fetch);
      return fetch.promise;
    });
    const first = store().refresh();
    await store().refresh();
    expect(fetches).toHaveLength(1);
    fetches[0].resolve(running());
    await first;
    expect(store().status?.phase).toBe("running");
  });

  test("fetches again when a command finishes during the fetch", async () => {
    // The poll's fetch is answered by the backend before the rename lands, but arrives after.
    const fetches: ReturnType<typeof deferred<Status>>[] = [];
    mockIPC((command) => {
      if (command !== "status") return;
      const fetch = deferred<Status>();
      fetches.push(fetch);
      return fetch.promise;
    });
    const poll = store().refresh();
    await store().run(() => Promise.resolve());
    expect(fetches).toHaveLength(1);

    fetches[0].resolve(running({ name: "Old name" }));
    await vi.waitFor(() => expect(fetches).toHaveLength(2));
    fetches[1].resolve(running({ name: "New name" }));
    await poll;
    expect(selectServer(store())?.name).toBe("New name");
  });
});

describe("run", () => {
  test("counts the command as pending until it settles", async () => {
    fakeBackend(running());
    await store().refresh();
    const command = deferred<string>();
    const result = store().run(() => command.promise);
    expect(store().pending).toBe(1);
    expect(selectBusy(store())).toBe(true);
    expect(selectReady(store())).toBe(false);

    command.resolve("done");
    expect(await result).toBe("done");
    expect(store().pending).toBe(0);
    expect(selectReady(store())).toBe(true);
  });

  test("passes the arguments through to the command", async () => {
    const backend = fakeBackend(running());
    const rename = (id: string, name: string) =>
      Promise.resolve(backend.calls.push({ command: "x", args: { id, name } }));
    await store().run(rename, "folder-1", "Films");
    expect(backend.calls).toEqual([
      { command: "x", args: { id: "folder-1", name: "Films" } },
    ]);
  });

  test("shows a failure as an error toast and returns FAILED", async () => {
    fakeBackend(running());
    const result = await store().run(() => Promise.reject("Disk full"));
    expect(result).toBe(FAILED);
    expect(store().toast).toEqual({ message: "Disk full", error: true });
    expect(store().pending).toBe(0);
  });

  test("refreshes afterwards, whether or not the command failed", async () => {
    const backend = fakeBackend(running({ paused: false }));
    await store().refresh();
    backend.status = running({ paused: true });
    await store().run(() => Promise.resolve());
    expect(selectServer(store())?.paused).toBe(true);

    backend.status = running({ paused: false });
    await store().run(() => Promise.reject("nope"));
    expect(selectServer(store())?.paused).toBe(false);
  });
});

describe("showToast", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  test("hides a message after 2.5 seconds and an error after 6", () => {
    store().showToast("Copied");
    expect(store().toast).toEqual({ message: "Copied", error: false });
    vi.advanceTimersByTime(2499);
    expect(store().toast).not.toBeNull();
    vi.advanceTimersByTime(1);
    expect(store().toast).toBeNull();

    store().showToast("Disk full", { error: true });
    vi.advanceTimersByTime(5999);
    expect(store().toast?.message).toBe("Disk full");
    vi.advanceTimersByTime(1);
    expect(store().toast).toBeNull();
  });

  test("a new toast gets its full time, not what was left of the old one", () => {
    store().showToast("First");
    vi.advanceTimersByTime(2000);
    store().showToast("Second");
    vi.advanceTimersByTime(1000);
    expect(store().toast?.message).toBe("Second");
    vi.advanceTimersByTime(1500);
    expect(store().toast).toBeNull();
  });
});

describe("pairings", () => {
  const now = Date.UTC(2026, 10, 15, 12);
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(now);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  test("addPairing notes when the request appeared and ignores repeats", () => {
    store().addPairing(pairingRequest({ pairingId: "a" }));
    store().addPairing(pairingRequest({ pairingId: "a" }));
    store().addPairing(pairingRequest({ pairingId: "b" }));
    expect(
      store().pairings.map((pairing) => pairing.request.pairingId),
    ).toEqual(["a", "b"]);
    expect(store().pairings[0].shown).toBe(now);
  });

  test("removePairing takes down only that request", () => {
    store().addPairing(pairingRequest({ pairingId: "a" }));
    store().addPairing(pairingRequest({ pairingId: "b" }));
    store().removePairing("a");
    expect(
      store().pairings.map((pairing) => pairing.request.pairingId),
    ).toEqual(["b"]);
  });

  test("a refresh takes down requests whose device has since paired", async () => {
    store().addPairing(
      pairingRequest({ pairingId: "ipad", clientName: "Kitchen iPad" }),
    );
    store().addPairing(
      pairingRequest({ pairingId: "tv", clientName: "Living Room" }),
    );
    // Within the slack: the device's clock or the event can run a little behind.
    fakeBackend(
      running({
        devices: [device({ clientName: "Kitchen iPad", pairedAt: now - 4000 })],
      }),
    );
    await store().refresh();
    expect(
      store().pairings.map((pairing) => pairing.request.pairingId),
    ).toEqual(["tv"]);
  });

  test("a device that paired well before the request doesn't answer it", async () => {
    store().addPairing(pairingRequest({ clientName: "Kitchen iPad" }));
    fakeBackend(
      running({
        devices: [device({ clientName: "Kitchen iPad", pairedAt: now - 6000 })],
      }),
    );
    await store().refresh();
    expect(store().pairings).toHaveLength(1);
  });
});

describe("selectors", () => {
  test("nothing is ready before the first status", () => {
    expect(selectReady(store())).toBe(false);
    expect(selectServer(store())).toBeNull();
  });

  test("nothing is ready unless the server is running", async () => {
    fakeBackend({
      phase: "starting",
      error: null,
      dataDir: "/data",
      server: null,
    });
    await store().refresh();
    expect(selectReady(store())).toBe(false);
  });
});
