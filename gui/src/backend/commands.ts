// The Tauri commands in `src-tauri/src/main.rs`, as typed functions.
//
// These only talk to the backend. Components call the ones that change something through
// `run` in the store, which disables the controls meanwhile, shows any error and
// refreshes the status afterwards.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { PairingRequest, Status } from "./types";

/** Everything the window shows. Polled once a second. */
export const fetchStatus = () => invoke<Status>("status");

// MARK: Sharing

export const setPaused = (paused: boolean) =>
  invoke<void>("set_paused", { paused });
/** An empty name means the computer's own name. */
export const setName = (name: string) => invoke<void>("set_name", { name });
export const setPairing = (on: boolean) => invoke<void>("set_pairing", { on });
export const setKeepAwake = (on: boolean) =>
  invoke<void>("set_keep_awake", { on });

// MARK: Folders

/** Asks for a folder and shares it; true if one was chosen. */
export const addFolder = () => invoke<boolean>("add_folder");
export const renameFolder = (id: string, name: string) =>
  invoke<void>("rename_folder", { id, name });
/** Asks first; true if it was removed. */
export const removeFolder = (id: string) =>
  invoke<boolean>("remove_folder", { id });
/** Checks one folder for changes now, or every folder for null. */
export const rescan = (id: string | null) => invoke<void>("rescan", { id });
export const openFolder = (id: string) => invoke<void>("open_folder", { id });
export const reindexEverything = () => invoke<void>("reindex_everything");

// MARK: Devices

/** Asks first; true if it was revoked. */
export const revokeDevice = (clientId: string) =>
  invoke<boolean>("revoke_device", { clientId });

/** Calls `handler` for each device that asks to pair. */
export function onPairingRequest(
  handler: (request: PairingRequest) => void,
): Promise<UnlistenFn> {
  return listen<PairingRequest>("pairing-request", (event) =>
    handler(event.payload),
  );
}

// MARK: Data folder

/** Opens the data directory, with the settings and index, in the file manager. */
export const showDataFolder = () => invoke<void>("show_data_folder");
