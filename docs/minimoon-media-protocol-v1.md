# MiniMoon Media Protocol (MMP) v1: proposal

Status: proposal, revised 2026-11-14. Nothing is built.

- **Server:** minimoon-media-server, the renamed minimoon-sync.
- **Clients:** MiniMoon (macOS, iOS, tvOS) and Matinee (tvOS, macOS).
- **Legacy:** the legacy sync protocol (`/files`, `/file-by-path` on port 41324) keeps running unchanged for older MiniMoon iOS clients. MMP is a separate protocol on its own port, with no compatibility with the legacy one.
- **Read-only:** clients never write anything to the server or its folders. Every slice and item route is a GET; the only POSTs are the optional pairing routes, which record a device in the server's own settings (§4). Watch progress, play counts, favourites and every other kind of per-user state stay on the client device, as they do today.

## 1. Why this exists: the advantage over Jellyfin, Navidrome and Plex

The project is worth doing only if it wins on something the incumbents can't easily copy.
Three claims hold up. Some common claims don't, and shouldn't be made.

**Real advantages**

1. **No administration.**
   - **MMP:** a desktop app with a folder picker and the Sync app's existing file-selection tree, where each saved selection is published as a slice (§8). The server appears on the TV over Bonjour and works with no code, account or password. An optional pairing code can be switched on (§4).
   - **Jellyfin:** its setup wizard creates an admin account and libraries in a web dashboard.
   - **Plex:** needs a plex.tv account.
   - **Navidrome:** runs as a service and is configured by hand.
   - **Who it reaches:** MiniMoon Sync users already installed this app on their Mac or PC and get it auto-updated, so they can be reached with no new install.
2. **Built for a full local copy.** One endpoint returns the whole library index in one response, and answers `304` when nothing has changed. Clients keep a complete local index, so browsing and search are instant, and the library can still be browsed while the host sleeps. A tvOS cache purge costs one re-download, not a rescan.
   - **Subsonic/OpenSubsonic:** has no efficient way to fetch everything. Offline-first clients like Symfonium rely on an empty-query `search3` that the spec doesn't define and that some servers don't support. I found no change feed.
   - **Jellyfin:** pages through item queries. Whether it has a general change feed is unverified.
3. **One tag reader end to end.** The server reads tags with the same `minimoon_tags` crate the MiniMoon apps use, and the clients do the interpretation (grouping, multi-artist credit splitting, cover choice). A library therefore looks the same on Mac, iPhone and TV.
   - **Jellyfin/Navidrome:** the TV would show the server's interpretation of the tags, which differs from what MiniMoon on the Mac shows.

**The other no-setup option: Apple's Home Sharing.** It's free and built in, and the Computers app on tvOS 26 still plays a Mac's shared library. It doesn't undercut advantage 1:
- It shares only Apple's Music and TV app libraries, not folders of files. The Music app doesn't play FLAC (from memory, not checked).
- Third-party apps can't use it.
- Users report it unreliable on tvOS 26 (forum posts, not confirmed by Apple).

**Not advantages (don't claim them)**

- **The host has to be awake.** That's true of every server here; only SMB on an always-on NAS avoids it.
- **No remote access, transcoding, multiple users, server-side metadata scraping or server-held watch state.** Jellyfin and Plex have all of these; MMP deliberately doesn't.
- **Cross-device watch progress.** The server doesn't store state, so it doesn't sync progress between Apple TVs. Matinee's plan for that is iCloud (`docs/monetization.md`), which also covers SMB shares and uploads.

**Who it's for:** people whose music and videos are on a Mac or PC and who would never run Jellyfin.
**Who it's not for:** homelab users who already run Jellyfin or Navidrome. Reach them later with Jellyfin and OpenSubsonic backends behind the same `LibrarySource` protocol in each app, not by out-featuring those servers.

## 2. Design principles

1. **The server serves facts; clients decide meaning.**
   - **The server sends:** files, folders, sizes, dates, audio properties and tags exactly as `minimoon_tags` reads them, and for video the container's stream facts and tags (§5).
   - **The clients keep:** grouping into albums, artists and shows, cover ranking, filename parsing, TheTVDB matching, and labels such as "4K", "Dolby Vision" or "5.1". These stay in the apps' existing Swift code.
   - **Why:** the server stays small and stable, and the logic isn't duplicated in two languages.
2. **Replica first.** Every client keeps a local copy, refreshed by downloading the full index when it changes. There are no browse, search, paging or delta endpoints. This is how MiniMoon's iOS sync already works against `/files`.
3. **Read-only.**
   - **No client writes:** no route uploads, edits, renames or deletes anything, or stores client data. Play counts, ratings, resume positions, track choices and Fix Match stay in each app's own on-device store.
   - **Nothing written into library folders:** the server writes only its own index, caches and settings, in its app-data directory. No tag edits, sidecars, thumbnails or hidden files next to the media.
   - **The one exception, when pairing is on:** pairing (§4) records a paired device in the server's own settings. That's the server's configuration, not the shared folders.
4. **One major version per path, additive changes within it.** Optional parts are listed in `features`. Clients ignore unknown fields and unknown enum values.
5. **Direct play only.** The server sends original bytes with Range support. Decoding is the client's job (AVFoundation in MiniMoon, VLCKit in Matinee).

## 3. Transport, discovery, versioning

- **Transport:** HTTP/1.1 (HTTP/2 allowed). JSON in UTF-8, gzip if the client accepts it.
- **Methods:** every `/v1` route is GET (and HEAD), except the two pairing routes in §4. The server answers any other method with `405`.
- **Port:** 41325 by default, separate from the legacy 41324.
- **Encryption:** plain HTTP in v1; see §9 for why and what replaces it later.
- **Bonjour:** the server advertises `_mmserver._tcp` (8 characters, under the 15-character limit). TXT records:
  - `id`: the server's UUID, persisted for the life of the install
  - `pv`: the protocol majors it supports, e.g. `1`
  - `name`: the display name
  - `auth`: `none` or `pairing`
- **Manual entry:** clients also accept a typed `host[:port]`.
- **Client identification:** every request carries `MMP-Client: <app>/<version> (<platform>)`, e.g. `matinee/1.3 (tvOS 26.1)`.
- **Too-old clients:** the server may answer `426` with `error.code = "client_too_old"` for a client version known to be broken.
- **Errors:** always `{"error": {"code": "<stable_snake_case>", "message": "<English, for logs>"}}` with a suitable HTTP status. Clients localize from `code`. v1 codes:
  - `not_paired`, `token_revoked` (only when pairing is on)
  - `pairing_expired`, `pairing_code_wrong`, `pairing_locked`
  - `not_found`, `range_not_satisfiable`, `method_not_allowed`
  - `folder_unavailable`
  - `client_too_old`, `unsupported_feature`

## 4. Access: open by default, optional pairing code

There is one household and no user accounts. Every client that can reach the server sees every slice.

**Open (`auth: none`, the default).** No token, code or password. Any device on the local network can list the slices and read the files in them, exactly as with the legacy Sync server today. Clients skip the pairing screen, and media URLs are plain `/v1/items/{id}/content` URLs that VLCKit and AVPlayer can open directly.

**Pairing (`auth: pairing`, a desktop-app setting: "Require a code for new devices").** A code rather than a password: the server shows six digits and the client asks for them once. There's nothing to remember or retype on each device, and still no account.

1. `POST /v1/pair` with `{"clientName": "Living Room", "app": "matinee", "platform": "tvOS"}`.
   - **Response:** `{"pairingId": "...", "expiresIn": 300}`.
   - **Server side:** the server shows a 6-digit code. The desktop app shows it in its window and a notification; the CLI prints it.
2. The user types the code on the client: `POST /v1/pair/{pairingId}/confirm` with `{"code": "123456"}`.
   - **Response:** `{"clientId": "...", "token": "<256-bit random, base64url>", "serverId": "..."}`.
   - **Limits:** 5 wrong codes lock that pairing. Pairing attempts are rate-limited per source address.
3. All other `/v1` requests send `Authorization: Bearer <token>`.
   - **Storage:** tokens are stored hashed in the server's settings, and in the Keychain on the client.
   - **Management:** the desktop app lists paired devices and can revoke any of them. A revoked token gets `401 token_revoked`.
4. **Signed media URLs**, for players that can't set headers (VLCKit, AVPlayer): `GET /v1/items/{id}/content-url` returns `{"url": "/v1/c/{id}?exp=...&sig=...", "expiresIn": 43200}`.
   - **Signature:** `sig` is an HMAC over the item id and expiry, made with a server key.
   - **Scope:** the URL grants only that item's bytes.

**Switching modes.** Turning pairing on makes connected clients get `401 not_paired`; each shows the pairing screen once. Turning it off makes the server ignore tokens.

`GET /v1/server` never needs a token; it's what a client calls first:

```json
{
  "id": "6f1c…", "name": "Living Room Mac mini",
  "auth": "none",
  "protocol": {"major": 1, "minor": 0, "features": ["art-resize", "events"]},
  "software": {"name": "minimoon-media-server", "version": "5.0.0"},
  "folders":   [{"name": "Music", "available": true},
                {"name": "Movies", "available": false}],
  "slices":    [{"id": "6b0e…", "name": "Everything", "kind": "mixed", "etag": "\"9f2c…\""},
                {"id": "2f9a…", "name": "Road Trip", "kind": "music", "etag": "\"41ab…\""},
                {"id": "c71d…", "name": "Movies", "kind": "video", "etag": "\"77d0…\""}]
}
```

- **One tree.** All shared folders are merged into a single tree, each appearing as a top-level folder under its display name (`folders`).
- **Slices** are named subsets of that tree, defined in the desktop app (§8). In v1 a slice is a selection of folders and files, like the Sync app's presets: one can take `Music/Jazz/` from one disk and `Movies/Concerts/` from another. Rule-based slices can come later without a protocol change, because clients never see how a slice is defined, only its files. Every slice is published at once.
- **Each device chooses its slice.** The choice is made in the client app and stored on that device, never on the server, so changing it on one device doesn't affect any other. This is what lets a MiniMoon user give each device its own set of files: "Everything" on the living-room Apple TV, "Road Trip" on a phone with little storage, "Kids" on a child's iPad. A client may follow more than one slice.
- **The choice stays hidden until there is one.** When the server publishes a single slice (typically "Everything", the whole tree), the client follows it without asking and shows no slice picker anywhere. When a second slice appears, the device keeps following what it already had, and the picker appears in the app's server settings; there is no prompt. Devices remember slices by `id`, so renaming one changes nothing for them. If the slice a device follows is removed, it falls back to the remaining one when only one is left, and otherwise asks.
- **Overlap:** every slice shares one path space, so a file in two slices has the same `id` and `path` in both. A client following both merges them by `path` rather than showing duplicates.

- **`kind`** is a hint for which app shows a slice (`music`, `video`, `mixed`). Items carry their own `kind`.
- **`folders[].available`** is false when that shared folder is missing, such as an unplugged drive or an unmounted network volume (§5). The slices stay listed; only the files under that folder go offline.
- **`etag`** is the current index ETag, so a client polling `/v1/server` can tell which slices changed without another request.
- **Pairing mode:** an unpaired client gets the same response with `folders` and `slices` omitted.

## 5. Slice sync: the full index, with an ETag

`GET /v1/slices/{sliceId}/index`

- **Response:** every folder and item in the slice, in one gzipped JSON response, with an `ETag`. Folders are included when they contain a file in the slice.

```json
{
  "folders": [ … every folder … ],
  "items":   [ … every item … ]
}
```

- **Unchanged slice:** the client sends `If-None-Match: <etag>`; the server answers `304 Not Modified` with no body.
- **ETag:** changes whenever the slice's files change, including the slice's definition being edited in the desktop app. The server derives it from its index (for example a hash, or a counter it bumps on every change); clients treat it as opaque.
- **Client update:** the client compares the new index with its local copy by its own key (below). Keys in the local copy but not in the index were removed, including files taken out of the slice; a changed `size` or `mtime` means the file changed; new keys were added. This is MiniMoon's iOS sync today (`SyncService.preparePlan`). Run the comparison off the main actor: on a large library it once blocked the main actor for seconds (MINIMOON-B9).
- **No deltas, cursors or tombstones.** Removals, moves, folder deletes and files added then removed between syncs all fall out of the client's comparison. The server keeps no change history.
- **Missing shared folder.** Before indexing, the server checks that each shared folder exists and is readable. If one isn't (an unplugged drive, an unmounted network volume, an empty mount point left behind):
  - it's marked `"available": false` in `/v1/server`;
  - the server keeps that folder's last index rather than replacing it with nothing, and slice indexes keep listing its files, with the response naming it in `"offline": ["Movies"]`;
  - requests for those files' content, art or lyrics answer `503 folder_unavailable`;
  - clients keep the files and show them as offline.
  Without this rule, a missing folder would look like every file under it had been deleted, and clients would drop them all. Availability changing changes the ETag of every slice with files there.
- **Slice removed in the desktop app:** it drops out of `/v1/server`, and `/index` answers `404 not_found`. Clients drop that slice.
- **Size (estimated, not measured):** the example audio item below is about 650 bytes of JSON, so 40,000 tracks is roughly 25 MB, a few MB gzipped. A video item is somewhat larger because of its streams, but video libraries hold far fewer files. Parsing and comparing a 100,000-track library on an Apple TV should be measured before relying on it. If it proves too slow, a delta endpoint can be added later as an optional feature, since v1 changes are additive.
- **Folder** (needed for folder views, cover ranking and Matinee's show-from-folder parsing):
  `{"id": "f3", "parentId": "f1", "name": "Kind of Blue (1959)", "path": "Miles Davis/Kind of Blue (1959)"}`
- **Item:**

```json
{
  "id": "i42",
  "folderId": "f3",
  "name": "01 So What.flac",
  "kind": "audio",
  "mime": "audio/flac",
  "size": 41234567,
  "mtime": 1710000000000,
  "audio": {"durationMs": 562000, "sampleRate": 96000, "bitDepth": 24,
            "bitrate": 2304, "channels": 2},
  "tags": { "title": "So What", "artist": "Miles Davis", "albumArtist": "Miles Davis",
            "album": "Kind of Blue", "trackNumber": 1, "trackNumberTotal": 5,
            "diskNumber": 1, "year": 1959, "genre": "Jazz",
            "sortArtist": null, "work": null, "compilation": false,
            "replaygainTrackGain": "-6.20 dB", "replaygainAlbumGain": "-7.01 dB",
            "rating": null, "hasLyrics": false, "hasEmbeddedArt": true }
}
```

- **Item kinds:** `audio`, `video`, `image`, `subtitle`, `lyrics`, `other`. Sidecars that already exist (`cover.jpg`, `.lrc`, `.srt`) are ordinary items in the same folder, and clients pair them.
- **`tags`:** the `minimoon_tags` `Tag` struct, camelCased, with the full `lyrics` text replaced by `hasLyrics` (fetched separately). Fields are strings and numbers exactly as read; nothing is split or normalized. `rating` is whatever rating the file's own tags hold; clients keep their own ratings locally. Audio items only; video items carry `video` and video `tags` instead (below).
- **Video item:** the container's stream facts and tags, read once per size or mtime change:

```json
{
  "id": "i97", "folderId": "f20", "name": "Show.S02E05.2160p.mkv",
  "kind": "video", "mime": "video/x-matroska", "size": 18734002176, "mtime": 1712000000000,
  "video": {
    "durationMs": 3134000, "container": "matroska",
    "streams": [
      {"type": "video", "codec": "hevc", "width": 3840, "height": 2160,
       "frameRate": 23.976, "transfer": "pq", "dolbyVision": 8, "hdr10Plus": false},
      {"type": "audio", "codec": "eac3", "channels": 6, "language": "eng",
       "title": "Surround 5.1", "default": true},
      {"type": "subtitle", "codec": "subrip", "language": "eng",
       "default": false, "forced": false}
    ]
  },
  "tags": {"title": "…", "show": "…", "season": 2, "episode": 5, "year": 2019,
           "description": null, "hasEmbeddedArt": false}
}
```

  - **`streams`:** every track in container order. Video streams give codec, size, frame rate and colour facts: `transfer` is `sdr`, `pq` or `hlg`, plus `dolbyVision` (the profile number, or `false`) and `hdr10Plus`. Audio gives codec and channels; subtitles give codec. Every stream gives language, title and default/forced flags where the container has them. Fields the reader can't determine are omitted, never guessed. Rules the reader test showed are needed:
    - **Colour comes from the codec headers.** `transfer` is read from the HEVC or H.264 SPS colour fields (VUI) or the AV1 sequence header. Matroska `Colour` elements are used only if present; none of 106 real MKVs had them.
    - **Dolby Vision** comes from the configuration record (Matroska `BlockAdditionMapping` or MP4 `dvcC`/`dvvC`). `dolbyVision` carries the profile number (e.g. `5`, `8`), not just a flag. Profile 5 implies PQ even though its base layer usually carries no transfer value.
    - **HDR10+** is only in SEI messages in the frames, so the reader checks the first HEVC frame.
    - **`frameRate`** is the nominal rate: Matroska `DefaultDuration`, or MP4 sample durations, falling back to timing the first frames. Variable-rate files report their nominal rate.
    - **`channels`** is the decoded channel count. HE-AAC with parametric stereo is stereo even though the container says 1.
    - **`language`** is BCP 47 when the container has it, otherwise its ISO 639-2 code. An absent Matroska Language means `eng`, per the Matroska spec.
  - **Video `tags`:** what the container itself holds. For MP4 that's the iTunes-style title, show, season, episode, year and description atoms; for Matroska, its title and tags. Missing fields are `null`. They're hints for Matinee's filename parsing and TheTVDB matching, not a replacement for either.
  - **Matinee's mapping:** `video` covers everything in Matinee's `ProbeResult`:
    - duration, size, video codec and frame rate;
    - dynamic range, from `transfer` and the flags;
    - the first audio stream's codec and channels;
    - audio and subtitle stream counts.
    Matinee fills `ProbeResult` from the index and skips its own VLCKit probe. That probe costs a network parse of up to 10 seconds per share file today, repeated after every tvOS cache purge. Per-track language and default/forced flags also let it apply audio and subtitle language preferences before playback.
  - **Unreadable files:** a container the reader can't parse gets no `video` object, and Matinee falls back to probing it with VLCKit as today.
- **Identity:**
  - **`path`:** the file's path in the unified tree, starting with its shared folder's display name (`Music/Miles Davis/Kind of Blue (1959)/01 So What.flac`). It's the same in every slice.
  - **`id`:** a hash of `path`, so it survives server index rebuilds, is the same in every slice, and changes only when the file moves or is renamed, or its shared folder's display name changes. Used in content, art and lyrics URLs.
  - **Clients key their on-device state by facts they already use:**
    - MiniMoon: the unified `path`, its track identity today. A moved file loses its state, as today, and so does every file under a shared folder whose display name is changed.
    - Matinee: `name` + `size`, the key it already gives share files (`ShareKeys.ownKey`), so its existing share machinery applies. A file moved to another folder keeps its state; a renamed one doesn't.
  - **No content fingerprint in v1.** A hash over the start of a file changes whenever a music file is retagged, because ID3 and FLAC tags sit at the front. Neither app needs one for server files. It can return as an optional feature if deduplication ever needs it.
  - **Content `ETag`:** derived from size and mtime.
- **Push:** with the `events` feature, `GET /v1/events` is a Server-Sent Events stream sending `slice-changed {sliceId, etag}`, `folder-availability {name, available}`, and `slices-changed` when a slice is added, removed or renamed (clients then re-read `/v1/server`). Clients that don't use it re-check `/v1/server` and `/index` with `If-None-Match` when they come to the foreground.

## 6. Content, artwork, lyrics

- **File bytes:** `GET /v1/items/{id}/content`, or in pairing mode the signed `/v1/c/{id}?exp&sig`.
  - Streams from disk with `Accept-Ranges: bytes`, `206` for ranges, `Content-Length`, the real `Content-Type`, and an `ETag` from size and mtime.
  - **MiniMoon:** downloads to its cache, then opens it with `AVAudioFile`.
  - **Matinee:** hands VLCKit the URL and streams directly. Seeking a long video through VLCKit's HTTP access against this server is untested.
- **Embedded artwork:** `GET /v1/items/{id}/art` returns the first embedded picture, or `404`. For video, that's a Matroska cover attachment or the MP4 `covr` atom.
- **Resized images** (`art-resize` feature): `?maxSize=600` scales either an item's embedded art or an `image` item's content to fit, as JPEG. Results are cached in the server's app-data directory, keyed by item id, mtime and size, never next to the media.
- **Embedded lyrics:** `GET /v1/items/{id}/lyrics` returns `text/plain`.

## 7. Client-side state (not part of the protocol)

The server stores nothing on behalf of clients. Each app keeps its state where it does today:
- **Matinee:** `PersistentRecord` in UserDefaults on the Apple TV (400 KB budget). A share file gets a record entry only once it has state worth keeping (watched, a position, a Fix Match or rename, a track choice); its automatic match lives in a purgeable cache sidecar. MMP files would follow the same rule.
- **MiniMoon:** its own library database on each device.
- **Syncing state between devices** is a separate decision outside MMP. Matinee's candidate is iCloud (`docs/monetization.md`).

## 8. Server behaviour

- **Shared folders and slices:**
  - **Shared folders** are the folders on disk the user shares, added in the desktop app or with CLI flags. The Sync app has one today; MMP allows several. They're merged into one tree, each as a top-level folder named after the folder on disk. The user can rename it, and a clash gets a number added (`Music 2`). Renaming changes the path of every file beneath, so the desktop app warns that MiniMoon will treat those files as new.
  - **Slices** are named subsets of that tree. In v1 every slice is a selection, edited in a file-selection tree that shows every shared folder at the top and works at any depth, as the Sync app's tree already does. Every slice is published; there is no "active" slice.
  - **Selections** pick whole folders, with everything under them, and single files, at any depth, as the Sync app's tree does. A whole shared folder is selected by selecting its top-level folder.
  - **`kind`** is chosen per slice in the desktop app, defaulting to the kind most of its files are.
  - **Legacy sync:** port 41324 still serves the one folder the Sync app shared, with paths relative to it as today. A desktop-app setting picks which slice it applies, limited to that folder, defaulting to the preset that was active at the upgrade. If the chosen slice isn't a `selection`, legacy sync uses the files it resolves to within that folder.
- **Slice definitions (persistence format).** Slices are stored in the server's settings, not in the Sync app's web storage (`localStorage`, in `state.ts`), because the server keeps running when the window is closed and the CLI needs them too.
  - **A typed syntax tree, not a query language.** A definition is a tree of typed nodes. Each node type has a fixed set of typed fields. Properties are named by closed enums, never by strings like `"tags.genre"`. Comparisons are typed per kind of value, never by operator strings. Paths are arrays of components, never strings with separators or trailing-slash conventions. Nothing in the format is parsed out of a string. The desktop app's editors build the tree directly.
  - **Normative definition** (Rust, as the server would declare it; JSON is its serde serialization, with node types tagged by `"type"` and names in camelCase):

```rust
struct SliceFile { format_version: u32, slices: Vec<Slice> }

struct Slice {
    id: Uuid,                 // created with the slice, never reused; the protocol's slice id
    name: String,
    kind: SliceKind,          // Music | Video | Mixed: the hint in /v1/server
    definition: SliceExpr,
}

/// A path in the unified tree, as components: ["Music", "Jazz"].
struct TreePath(Vec<String>);

enum SliceExpr {
    // v1
    All,                                                 // every file in every shared folder
    Selection { folders: Vec<TreePath>, files: Vec<TreePath> },

    // reserved for rule-based slices; not implemented in v1
    And { children: Vec<SliceExpr> },
    Or { children: Vec<SliceExpr> },
    Not { child: Box<SliceExpr> },
    Text { field: TextField, test: TextTest, case_sensitive: bool },
    Number { field: NumberField, test: NumberTest },
    Date { field: DateField, test: DateTest },
    Flag { field: FlagField, value: bool },
    Kind { kinds: Vec<ItemKind> },                       // Audio | Video | Image | ...
    Transfer { values: Vec<Transfer> },                  // Sdr | Pq | Hlg
    Limit { child: Box<SliceExpr>, sort: SortKey, order: Order, count: u32 },
}

enum TextField   { Title, Artist, AlbumArtist, Album, Genre, Work, FileName }
enum NumberField { Year, TrackNumber, DiskNumber, DurationMs, SampleRate, BitDepth,
                   Channels, Width, Height, SizeBytes }
enum DateField   { Modified }
enum FlagField   { Compilation, HasLyrics, HasEmbeddedArt, DolbyVision, Hdr10Plus }

enum TextTest   { Is { value: String }, Contains { value: String },
                  StartsWith { value: String } }
enum NumberTest { Equals { value: i64 }, LessThan { value: i64 },
                  GreaterThan { value: i64 }, Between { min: i64, max: i64 } }
enum DateTest   { Before { at_ms: i64 }, After { at_ms: i64 }, WithinLastDays { days: u32 } }
```

  - **v1 files:**

```json
{
  "formatVersion": 1,
  "slices": [
    {"id": "6b0e…", "name": "Everything", "kind": "mixed", "definition": {"type": "all"}},
    {"id": "2f9a…", "name": "Road Trip", "kind": "music",
     "definition": {"type": "selection",
                    "folders": [["Music", "Jazz"], ["Music", "Rock", "Classics"]],
                    "files": [["Music", "Misc", "song.flac"]]}}
  ]
}
```

  - **A later rule-based definition** (illustrative; not v1): jazz from the 1950s and 60s under `Music`:

```json
{"type": "and", "children": [
  {"type": "selection", "folders": [["Music"]], "files": []},
  {"type": "text", "field": "genre", "test": {"type": "contains", "value": "Jazz"}, "caseSensitive": false},
  {"type": "number", "field": "year", "test": {"type": "between", "min": 1950, "max": 1969}}
]}
```

  - **Fields map to index facts, one each.** Every field variant names exactly one fact from §5 (`Genre` is `tags.genre`, `Width` is the first video stream's width, `Modified` is `mtime`). That mapping lives in the server's code, not in the file. A file whose fact is missing doesn't match a test on it.
  - **Extending** means adding variants: new node types, fields or tests. That doesn't change `formatVersion`, which only changes if an existing type's meaning changes.
  - **Strict parsing, fail closed.** Each slice is parsed on its own, rejecting unknown variants and unknown fields. A slice that doesn't parse (made by a newer server, say) isn't published at all, never published with a guess. The desktop app shows it read-only as needing a newer version. The server stores it verbatim and writes back only the slices it parsed, so an older server never drops or alters a newer one's definitions. A file with a newer `formatVersion` is treated the same way, as a whole.
  - **Evaluation** happens on the server against its index. A slice's membership, and so its ETag, changes when its definition changes or when a file's facts change. For a future rule slice that includes a retag that moves a song in or out. Clients never see definitions, only `/v1/server` entries and indexes, so new node types need no client change.
  - **Migration from the Sync app.** On first launch after the upgrade, the app hands its existing presets to the server, and the server converts each to a `selection`. The Sync app stores a preset as path strings in which a trailing `/` marks a folder. The migration is the one place that convention is read: entries ending in `/` become `folders`, the rest `files`, each split into components with the shared folder's display name added in front (`Jazz/` becomes `["Music", "Jazz"]`). A preset with an empty selection becomes `all`, so "Default" keeps covering everything, including folders shared later.
- **Enforcement:** an item is served (`/content`, `/art`, `/lyrics`) only if it's in at least one published slice; otherwise it gets `404`, as if it didn't exist. Item routes aren't scoped by slice, because every client can see every slice. Slices are enforced by the server on every route, not only in listings. (The legacy server applies it only to `/files`; that's left as is, since the legacy API is barely used.)
- **Index:** a SQLite database in the app's data directory, holding the shared folders, files, folders and tags. Each file is indexed once, however many slices include it. Slice definitions and, when pairing is on, paired devices live in the server's settings. It exists so tags aren't re-read on every request; the legacy `/files` walks the whole tree on every call. The server may also keep each slice's gzipped index response ready, rebuilt when the slice changes.
- **Change detection:**
  - File-system watching, plus a full rescan at startup and periodically.
  - Only files whose size or mtime changed are read again.
  - A rescan of a missing or empty-looking shared folder marks that folder offline instead of removing its files (§5).
- **Sleep:** while any `/content` response is streaming, the server holds a "prevent idle sleep" assertion on macOS (and the Windows equivalent), plus an optional "Keep this computer awake while sharing" setting. Clients show a distinct "server asleep or off" state and keep browsing from their local copy.
- **Network visibility:** the desktop app shows "Sharing on ‹network›" and has a one-click pause, because in open mode a laptop shares its slices with any network it joins (§9).
- **Legacy:** the old sync routes stay on 41324 as they are, unfixed, because they're barely used now. They can be removed once older clients are gone.

## 9. Security in v1, and what's deferred

- **Open by default.** Anyone on the same network can read the chosen folders. That's the same exposure as the legacy Sync server, which has no auth.
  - **The real risk is a laptop that moves:** with the server running, a MacBook on hotel or café Wi-Fi advertises itself and serves its slices to everyone there.
  - **v1 mitigation:** the network name and pause control in the desktop app (§8), and the optional pairing code.
  - **Possible later:** remember the network chosen at setup and pause sharing on any other. Reading the Wi-Fi name on recent macOS may need Location permission (from memory, not checked).
- **Slices are not access control.** They decide what's published, not who can see it: every client can see every slice. A "Kids" slice hides nothing from any other device. Files outside every slice are never served.
- **Nothing to damage.** Because no route writes, an unwanted client can read the chosen folders but can't change, delete or add anything, or alter another device's state.
- **Pairing mode:** a code shown on the server proves consent, and every API call and media URL is authenticated.
- **Plain HTTP.** Media, and in pairing mode the token, can be sniffed by anyone on the same LAN.
  - **Why plain:** TLS with a self-signed, pinned certificate works in URLSession through its challenge delegate, but it's unverified for VLCKit's HTTP access and for AVPlayer without a resource loader.
  - **Later:** a `tls` feature with the certificate fingerprint pinned at pairing, once VLCKit's behaviour is tested.
- **Inside chosen folders and selections only.** The server never exposes paths outside the shared folders or outside every slice, filters hidden files, rejects path traversal, and never follows symlinks out of the shared folders unless the user opts in. This matters more in open mode. The legacy `/file` route skips some of these filters, so it's not a model to copy.

## 10. Smallest first version

**Core (protocol 1.0)**

- **Server:**
  1. Rename and rebrand the server; leave the legacy routes in place. Keep the bundle identifier (`com.plastaq.sync-server`) and the updater endpoint so existing installs upgrade in place. Whether the Tauri updater handles a product-name change cleanly is unverified.
  2. `/v1/server` and Bonjour advertising, open mode only.
  3. SQLite index with tags from the `minimoon_tags` crate, the missing-folder check, and `/index` with ETag and `304`. This is the largest server piece.
  4. `/content` with Range, `/art`, `/lyrics`.
  5. Desktop app UI: shared folders and their display names, slices (the existing selection tree spanning every shared folder, minus the active-preset switch), the legacy-sync slice setting, sharing network and pause. Store slices in the server's settings in the format in §8 (`all` and `selection` only), migrating existing presets.
  6. A `minimoon_tags` change: duration in milliseconds (it's whole seconds today, `properties.duration().as_secs()`) and a channel count.
  7. **Video reading in native Rust**, chosen by the reader test ([video-reader-test.md](video-reader-test.md), 2026-11-14):
     - **Crates:** `matroska-demuxer`, `mp4-atom`, `scuffle-h265`, `h264-reader`, `scuffle-av1`, and `lofty` for MP4 tags.
     - **Hand-written parsing:** about 240 lines, covering the Dolby Vision configuration record, Matroska attachment headers, raw EBML and MP4 box scanning (`mp4-atom` drops `dvcC`/`dvvC`), and the HDR10+ SEI check.
     - **Results:** on 107 real files and 10 synthetic HDR clips it matched ffprobe and MediaInfo, and detected every HDR10, HLG, HDR10+ and Dolby Vision 5 and 8.1 case. It ran in a median of 19 ms and read a median of 100 KB per file. ffprobe read 5 MB per file and would need a custom LGPL build; MediaInfoLib is an 8 MB C++ library needing FFI.
     - **Still to do:** read MP4 forced and default flags and track titles; report HE-AAC parametric stereo as 2 channels.
     - **Placement:** it goes in the server, or in `minimoon_tags` if the apps might ever want it on-device.
- **Shared Swift client package:** used by both MiniMoon and Matinee. Covers offline shared folders, discovery (`NWBrowser`), the per-device slice picker (choice stored on the device by slice `id`, with overlapping slices merged by `path`), fetching `/index` with `If-None-Match`, the off-main-actor comparison against the app's own store, content URLs, and an error mapping. Matinee and MiniMoon are separate repos, so this is a new Swift package with its own repo.
- **MiniMoon:** an MMP `LibrarySource` mapping items in the device's chosen slices to `Track` rows, plus fetch-to-cache playback. Switching slice on a device removes the files that are no longer in it from that device, through the normal index comparison.
- **Matinee:** an MMP source feeding `LibraryStore` the way a share does, filling `ProbeResult` from each item's `video` facts instead of probing. It plays content URLs through VLCKit. Its existing share machinery (record keys, the state-worth-keeping rule, offline state, the title cap) applies.

**Next (protocol 1.1, already designed above)**

- Optional pairing code, tokens and signed URLs (server setting, client pairing screen, Keychain storage).
- `events` (SSE).
- `art-resize`.

**Later, separate decisions**

- TLS.
- Pausing on unrecognised networks.
- Jellyfin and OpenSubsonic client backends in the apps.
- Chapters and more video facts (bit depth, bitrate per stream), added to `streams` as optional fields.

**Out of scope for MMP:** any write route, including state sync. Cross-device state, if wanted, goes through iCloud or another channel the apps own.

## 11. Verified vs unverified

**Verified**

- **Code:**
  - The `minimoon_tags` `Tag` and `Properties` fields: single-string artist, ReplayGain gains only, duration in whole seconds, no channel count.
  - Matinee's watch state is on-device only: `PersistentRecord` in UserDefaults with a 400 KB budget (`PersistentRecord.swift`). Share files get an entry only once they have state worth keeping (`LibraryStore+Shares.swift`).
  - Matinee keys share files by name + size (`ShareScanner.swift`, `ShareKeys.ownKey`); uploads add a first/last-MB hash (`FileHashing.swift`).
  - Matinee already plays and lists SMB through VLCKit 4.
  - Running Matinee's prober code over 107 real files, its VLCKit path never set a dynamic range, so MKV and share files get no HDR or Dolby Vision label. It also missed the frame rate for 5 files and reported HE-AAC parametric stereo as mono ([video-reader-test.md](video-reader-test.md)).
  - The video reader test: native Rust, ffprobe and MediaInfo agree on duration, codec, size, transfer, Dolby Vision, HDR10+ and covers for all 107 real files. All three were correct on 10 synthetic HDR clips ([video-reader-test.md](video-reader-test.md)).
  - Matinee probes each video once and caches a `ProbeResult` in a purgeable sidecar (`Playback.swift`, `LibraryStore.swift`). It holds duration, size, video codec, dynamic range, frame rate, the first audio track's codec and channels, and audio and subtitle track counts. Local MP4 and MOV files go through AVFoundation; everything else, including share files, goes through a VLCKit parse with a 10-second limit (`VLCKitProber` in `VLCKitEngine.swift`).
  - The Sync app's selection: a tree of folders and files saved as named presets in web storage, one active at a time (`minimoon-sync/src/state.ts`). Folder node ids end in `/`. The server receives the active preset as `selected_roots` and filters `/files` by path prefix, with an empty list meaning everything (`list_files_handler`). `/file-by-path` (`download_file_query_handler`) and the deprecated `/file` don't apply it.
  - MiniMoon's iOS sync already downloads the full `/files` list and compares by library-relative path, size and mtime (`SyncService.swift`); the comparison once blocked the main actor on a large library (MINIMOON-B9).
  - The legacy server's routes, and that it has no auth.
  - `lofty` reads no video fields and fails on Matroska; it reads MP4 tags and covers correctly (video reader test).
- **Apple and competitors:**
  - **Local network privacy:** tvOS doesn't have it (TN3179).
  - **Plex:** remote playback of personal video needs Plex Pass or a Remote Watch Pass since April 2025, enforced in stages. Local playback is unaffected.
  - **Jellyfin:** its setup wizard creates an admin account. The server is GPL-2.0.
  - **Swiftfin:** the Jellyfin tvOS client is video-only, per its 2022 announcement and a 2026 third-party guide. Its current release isn't checked.
  - **Subsonic:** clients use an empty `search3` query for a full sync.
  - **Home Sharing:** the Computers app is still documented for tvOS 26.

**Unverified**

- Whether Jellyfin or OpenSubsonic have a change feed.
- What users actually use Sync app presets for. Publishing each as a slice fits a per-device subset; it changes behaviour for anyone who switches presets to change what every device sees.
- Index size and the time to parse and compare it on an Apple TV for a 100,000-track library (only estimated).
- VLCKit with self-signed TLS.
- VLCKit streaming and seeking over this server's HTTP routes.
- The Tauri updater across a product-name change.
- The Music app's lack of FLAC support (from memory).
- The video reader on real Dolby Vision and HDR10+ releases (only synthetic clips were tested), on Dolby Vision profile 7, on iTunes MP4s, AVI, MPEG-TS and WebM, over network volumes, and on Windows.
- Whether reading the Wi-Fi name on macOS needs Location permission (from memory).
- Whether Bonjour Sleep Proxy wakes the host on access.
- Wake-on-LAN from tvOS.
- `notify`-crate reliability on network volumes the host itself mounts.
