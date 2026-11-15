// The window's shared state, in one zustand store: the server's status, the commands in
// flight, the toast and the pairing requests on screen, with the actions that change them.
//
// Components read it with `useStore(selector)` and the selectors below. State only one
// component cares about (a rename being typed, a menu being open) stays in that component.

import { create } from "zustand";
import * as commands from "./backend/commands";
import type { DeviceView, PairingRequest, Status } from "./backend/types";

/** What `run` returns when the command failed; the error has already been shown. */
export const FAILED = Symbol("failed");
export type Outcome<T> = T | typeof FAILED;

export interface Toast {
  message: string;
  /** Red, and stays up longer. */
  error: boolean;
}

/** A pairing request on screen. */
export interface Pairing {
  request: PairingRequest;
  /** When it appeared, in milliseconds since the Unix epoch. */
  shown: number;
}

interface State {
  /** The last status, or null before the first fetch returns. */
  status: Status | null;
  /** Commands in flight. Controls stay disabled until they finish. */
  pending: number;
  toast: Toast | null;
  pairings: Pairing[];

  /** Fetches the status. Called once a second, and after every command. */
  refresh: () => Promise<void>;
  /**
   * Runs a command from `backend/commands.ts` with `args`. Controls are disabled until it
   * finishes, an error shows as a toast, and the status is refreshed afterwards.
   */
  run: <Args extends unknown[], T>(
    command: (...args: Args) => Promise<T>,
    ...args: Args
  ) => Promise<Outcome<T>>;
  /** Shows a toast, replacing any already up. */
  showToast: (message: string, options?: { error?: boolean }) => void;
  /** Shows a device's request to pair, unless it's already showing. */
  addPairing: (request: PairingRequest) => void;
  /** Takes a pairing request down: dismissed, expired or paired. */
  removePairing: (pairingId: string) => void;
}

/** How long a toast stays up, in milliseconds. */
const TOAST_DURATION = { normal: 2500, error: 6000 };
/** A device paired up to this long before its request appeared still counts as answering it. */
const PAIRED_SLACK_MS = 5000;

// Bookkeeping that nothing renders, so it lives outside the store.
/** Commands finished so far: a status fetched before one finished is stale. */
let finished = 0;
let refreshing = false;
let toastTimer: ReturnType<typeof setTimeout> | undefined;

export const useStore = create<State>()((set, get) => ({
  status: null,
  pending: 0,
  toast: null,
  pairings: [],

  refresh: async () => {
    // The fetch already in flight starts over if a command finishes meanwhile.
    if (refreshing) return;
    refreshing = true;
    let status: Status;
    try {
      let started;
      do {
        started = finished;
        try {
          status = await commands.fetchStatus();
        } catch (error) {
          status = {
            phase: "failed",
            error: String(error),
            dataDir: "",
            server: null,
          };
        }
      } while (started !== finished);
    } finally {
      refreshing = false;
    }
    const devices = status.server?.devices ?? [];
    set((state) => ({
      status,
      // A request whose device now shows up as paired has been answered.
      pairings: state.pairings.filter(
        (pairing) => !isAnswered(pairing, devices),
      ),
    }));
  },

  run: async (command, ...args) => {
    set((state) => ({ pending: state.pending + 1 }));
    try {
      return await command(...args);
    } catch (error) {
      get().showToast(String(error), { error: true });
      return FAILED;
    } finally {
      set((state) => ({ pending: state.pending - 1 }));
      finished++;
      await get().refresh();
    }
  },

  showToast: (message, { error = false } = {}) => {
    clearTimeout(toastTimer);
    set({ toast: { message, error } });
    toastTimer = setTimeout(
      () => set({ toast: null }),
      error ? TOAST_DURATION.error : TOAST_DURATION.normal,
    );
  },

  addPairing: (request) => {
    if (
      get().pairings.some(
        (pairing) => pairing.request.pairingId === request.pairingId,
      )
    )
      return;
    set((state) => ({
      pairings: [...state.pairings, { request, shown: Date.now() }],
    }));
  },

  removePairing: (pairingId) =>
    set((state) => ({
      pairings: state.pairings.filter(
        (pairing) => pairing.request.pairingId !== pairingId,
      ),
    })),
}));

function isAnswered(pairing: Pairing, devices: readonly DeviceView[]): boolean {
  return devices.some(
    (device) =>
      device.clientName === pairing.request.clientName &&
      device.pairedAt >= pairing.shown - PAIRED_SLACK_MS,
  );
}

// MARK: Selectors

/** Whether controls can be used: the server is running and no command is in flight. */
export const selectReady = (state: State) =>
  state.status?.phase === "running" && state.pending === 0;

/** Whether a command is in flight. */
export const selectBusy = (state: State) => state.pending > 0;

/** The running server's view, or null while starting or after a failed start. */
export const selectServer = (state: State) => state.status?.server ?? null;
