// What the Rust side sends. These mirror the serde structs in `src-tauri/src/main.rs`
// (`Status`, `ServerView`, `FolderView`, `DeviceView`, `PairingEvent`), with camelCase names.

/** Everything the window shows, fetched with the `status` command once a second. */
export interface Status {
  phase: "starting" | "failed" | "running";
  /** Why the server couldn't start, when `phase` is "failed". */
  error: string | null;
  /** The data directory, with the settings and index. */
  dataDir: string;
  /** Only while `phase` is "running". */
  server: ServerView | null;
}

export interface ServerView {
  /** The name devices see. */
  name: string;
  /** The saved name, or null for the host name. */
  customName: string | null;
  /** This computer's own name, which devices see when there's no saved name. */
  hostName: string;
  /** Whether new devices need a pairing code. */
  pairing: boolean;
  keepAwake: boolean;
  paused: boolean;
  /** Like "http://192.168.1.24:8787"; null while paused, since pausing closes the listener. */
  address: string | null;
  /** Streams playing now. */
  downloads: number;
  folders: FolderView[];
  devices: DeviceView[];
}

export interface FolderView {
  id: string;
  name: string;
  path: string;
  /** Whether devices can see it now. */
  shared: boolean;
  /** Why it isn't shared: "Not shared until it's indexed", "Not shared: …" or "Offline: …". */
  note: string | null;
  /** What indexing is doing now, like "Reading new files". */
  activity: string | null;
  /** Done and total for `activity`, when it's countable. */
  progress: [done: number, total: number] | null;
  files: number;
  /** Files whose tags or video details couldn't be read. */
  unreadable: number;
  /** "path: message" for each item the last scan couldn't read. */
  scanErrors: string[];
}

export interface DeviceView {
  clientId: string;
  clientName: string;
  /** The client app, like "MiniMoon". */
  app: string;
  /** Free-form, as the app reports it: "iOS", "tvOS", "Android TV", "macOS"… */
  platform: string;
  /** Milliseconds since the Unix epoch. */
  pairedAt: number;
}

/** The `pairing-request` event: a device asking to connect, and the code to enter on it. */
export interface PairingRequest {
  pairingId: string;
  /** Six digits. */
  code: string;
  clientName: string;
  app: string;
  platform: string;
  expiresInSecs: number;
}
