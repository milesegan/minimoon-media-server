// Sample data shaped like the Rust side's, and a fake backend built on Tauri's `mockIPC`.

import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  DeviceView,
  FolderView,
  PairingRequest,
  ServerView,
  Status,
} from "../backend/types";

export function folder(fields: Partial<FolderView> = {}): FolderView {
  return {
    id: "folder-1",
    name: "Music",
    path: "/Users/me/Music",
    shared: true,
    note: null,
    activity: null,
    progress: null,
    files: 1200,
    unreadable: 0,
    scanErrors: [],
    ...fields,
  };
}

export function device(fields: Partial<DeviceView> = {}): DeviceView {
  return {
    clientId: "client-1",
    clientName: "Living Room",
    app: "MiniMoon",
    platform: "tvOS",
    pairedAt: Date.now(),
    ...fields,
  };
}

export function server(fields: Partial<ServerView> = {}): ServerView {
  return {
    name: "Studio Mac",
    customName: null,
    hostName: "Studio Mac",
    pairing: true,
    keepAwake: false,
    paused: false,
    address: "http://192.168.1.24:8787",
    downloads: 0,
    folders: [folder()],
    devices: [],
    ...fields,
  };
}

/** A running server's status, with `fields` replacing the server's. */
export function running(fields: Partial<ServerView> = {}): Status {
  return {
    phase: "running",
    error: null,
    dataDir: "/Users/me/Library/Application Support/MiniMoon",
    server: server(fields),
  };
}

export function pairingRequest(
  fields: Partial<PairingRequest> = {},
): PairingRequest {
  return {
    pairingId: "pairing-1",
    code: "482913",
    clientName: "Kitchen iPad",
    app: "MiniMoon",
    platform: "iPadOS",
    expiresInSecs: 120,
    ...fields,
  };
}

export interface Call {
  command: string;
  args: Record<string, unknown>;
}

export interface FakeBackend {
  /** What the `status` command returns; tests change it to move the server along. */
  status: Status;
  /** Every command but `status`, in order, with its arguments. */
  calls: Call[];
}

/**
 * Answers `status` with `backend.status` and records every other command. A command in
 * `handlers` returns (or throws) what its handler does; the rest return nothing. Event
 * mocking is on, so `emit` from `@tauri-apps/api/event` reaches `listen` handlers.
 */
export function fakeBackend(
  status: Status,
  handlers: Record<string, (args: Record<string, unknown>) => unknown> = {},
): FakeBackend {
  const backend: FakeBackend = { status, calls: [] };
  mockIPC(
    (command, payload) => {
      if (command === "status") return backend.status;
      const args = (payload ?? {}) as Record<string, unknown>;
      backend.calls.push({ command, args });
      return handlers[command]?.(args);
    },
    { shouldMockEvents: true },
  );
  return backend;
}
