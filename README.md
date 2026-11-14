# MiniMoon Media Server

The LAN media server behind the MiniMoon and Matinee apps. It shares folders of music and
video with devices on the same network over the MiniMoon Media Protocol (MMP) v1, specified in
[docs/minimoon-media-protocol-v1.md](docs/minimoon-media-protocol-v1.md).

- **One index per slice, one request.** Clients download a slice's whole index, with a weak
  ETag and `304 Not Modified`, and keep a local copy.
- **Facts, not interpretation.** Tags are read with a copy of the apps' own tag reader, and
  video stream facts (codecs, HDR, Dolby Vision, tracks) with a native Rust reader.
- **Read-only.** Nothing is ever written into the shared folders.
- **No accounts.** Open to the local network by default, with an optional six-digit pairing
  code for new devices.

This replaces the Sync server's protocol (`/files`, `/file-by-path` on port 41324), which is
gone. The apps handle the move to MMP.

## CLI

```sh
cargo install minimoon-media-server
minimoon-media-server ~/Music ~/Movies
```

From a source checkout:

```sh
cargo run --release -- ~/Music ~/Movies
```

Folders given on the command line are added to the saved shared folders, so later runs need no
arguments. On a fresh install the server publishes one slice, "Everything". The server prints a
summary, then a line once the first scan has indexed everything, and a pairing code whenever a
device asks to pair.

Options:

- `--data-dir <dir>`: where the settings, slices, index and art cache live. The default is
  `com.plastaq.sync-server/media-server` in the platform's app-data folder (on macOS,
  `~/Library/Application Support`).
- `--port <port>`: the port to listen on, 41325 by default.
- `--bind <address>`: the address to listen on; every IPv4 interface by default.
- `--name <name>`: the display name devices see (saved). The host name by default.
- `--pairing` / `--open`: require a pairing code for new devices, or let any device on the
  network connect (saved).
- `--exclude <subfolder>` / `--include <subfolder>`: publish a shared folder without one of
  its subfolders, or undo that (saved). A folder with a subfolder the server can't read isn't
  published until the subfolder is readable or excluded; the server prints which paths
  failed.
- `--follow-external-symlinks`: follow links that point outside the shared folders. Links
  inside them are always followed.
- `--no-bonjour`: don't advertise `_mmserver._tcp` over Bonjour.
- `--reindex`: re-read every file's metadata in the background.
- `-v`, `--verbose`: log requests and scans.

## HTTP API

All routes are under `/v1`; see the spec for the full contract.

| Route | |
|---|---|
| `GET /v1/server` | Server id, name, auth mode, protocol version and features, shared folders, published slices with their ETags |
| `GET /v1/slices/{id}/index[?view=files]` | A slice's full index, or its file list |
| `GET /v1/items/{id}/content` | File bytes, with ranges, `If-Range` and `If-None-Match` |
| `GET /v1/items/{id}/art[?maxSize=N]` | The first embedded picture, optionally resized to JPEG |
| `GET /v1/items/{id}/lyrics` | Embedded lyrics as plain text |
| `GET /v1/events` | Server-Sent Events: `slice-changed`, `folder-availability`, `slices-changed` |
| `POST /v1/pair`, `POST /v1/pair/{id}/confirm` | Pairing (when pairing is on) |
| `GET /v1/items/{id}/content-url`, `GET /v1/c/{id}?…` | Signed media URLs for players that can't send headers |

## How it works

- **Settings** (`settings.json`): the server id, shared folders with their UUIDs, display names
  and volume mount points, bumped `rev` values, the last FSEvents event id, and pairing state.
  Slice definitions are in `slices.json`. Neither can be rebuilt from the disks, so neither is
  ever replaced silently.
- **Index** (`index.sqlite`): each shared folder's last published snapshot, with every file's
  metadata, reader version and read status. Deleting it loses nothing that the next scan
  doesn't read again; ids and `rev` values come back unchanged.
- **Scanning:** a full scan at startup and every 30 minutes, and a debounced rescan after file
  events. Before and after each walk the folder's volume is checked, so an unplugged drive
  never looks like deleted files. A walk with any error publishes nothing for that folder, and
  a completed scan is published in one step, once the metadata of every new or changed file
  has been read.
- **`rev`:** a file written in place with its size and mtime unchanged (some taggers put the
  old mtime back) gets a new `rev` when the watcher reported the write and the file's ctime
  moved. On macOS the server runs its own FSEvents stream and resumes from the stored event id,
  so writes made while it was stopped are replayed. On Linux and Windows it uses the `notify`
  crate.
- **Metadata:** audio through `src/reader/audio.rs`, a read-only copy of the apps'
  `minimoon_tags` reading code over `lofty`; video through `src/reader/video/`, built on
  `matroska-demuxer`, `mp4-atom`, `scuffle-h265`, `h264-reader` and `scuffle-av1`. Reads run on
  worker threads with a time limit. Temporary failures are retried with backoff, and files read
  by an older reader version are re-read in the background.
- **Sleep:** while content is streaming, and for ten minutes after, the server holds an
  idle-sleep assertion on macOS and Windows.

## Library use

The desktop app embeds the server through `MediaServer`:

```rust
use minimoon_media_server::{LibraryOptions, MediaServer, ServerOptions};

let server = MediaServer::start(ServerOptions::new(LibraryOptions::new(data_dir))).await?;
let folder = server.library().add_shared_folder(path, None)?;
server.library().ensure_default_slice()?;
```

`Library` covers shared folders (add, rename, remove, publish without unreadable subfolders),
slices (create, update, publish, delete, companion previews, migrating the Sync app's presets),
"Reindex everything" and per-folder status with error counts. `MediaServer` adds pausing,
pairing mode, paired devices and revocation, the display name and "Keep this computer awake".
