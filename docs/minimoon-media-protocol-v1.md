# MiniMoon Media Protocol (MMP) v1: proposal

Status: proposal, revised 2026-11-14 after review. Nothing is built.

- **Server:** minimoon-media-server, the renamed minimoon-sync.
- **Clients:** MiniMoon (macOS, iOS, tvOS) and Matinee (tvOS, macOS).
- **Legacy:** the legacy sync protocol (`/files`, `/file-by-path` on port 41324) keeps running for older MiniMoon iOS clients while pairing is off (§8). MMP is a separate protocol on its own port, with no compatibility with the legacy one.
- **Read-only:** clients never write anything to the server or its folders. Every slice and item route is a GET; the only POSTs are the pairing routes (protocol 1.1), which record a device in the server's own settings (§4). Watch progress, play counts, favourites and every other kind of per-user state stay on the client device, as they do today.

## 1. Why this exists: the advantage over Jellyfin, Navidrome and Plex

The project is worth doing only if it wins on something the incumbents can't easily copy.
Three claims hold up. Some common claims don't, and shouldn't be made.

**Real advantages**

1. **No administration.**
   - **MMP:** a desktop app with a folder picker and the Sync app's existing file-selection tree, where saved selections become slices (§8). The server appears on the TV over Bonjour and works with no code, account or password. An optional pairing code comes in protocol 1.1 (§4).
   - **Jellyfin:** its setup wizard creates an admin account and libraries in a web dashboard.
   - **Plex:** needs a plex.tv account.
   - **Navidrome:** runs as a service and is configured by hand.
   - **Who it reaches:** MiniMoon Sync users already installed this app on their Mac or PC and get it auto-updated, so they can be reached with no new install.
2. **Built for a full local copy.** One endpoint returns the whole library index in one response, and answers `304` when nothing has changed. Clients keep a complete local index, so browsing and search are instant, and the library can still be browsed while the host sleeps. A tvOS cache purge costs one re-download, not a rescan.
   - **Subsonic/OpenSubsonic:** has no efficient way to fetch everything. Offline-first clients like Symfonium rely on an empty-query `search3` that the spec doesn't define and that some servers don't support. I found no change feed.
   - **Jellyfin:** pages through item queries. Whether it has a general change feed is unverified.
3. **One tag reader end to end.** The server reads tags with the same code the MiniMoon apps use: a copy of the reading side of their `minimoon_tags` crate, which is a thin layer over `lofty` (§8). The clients do the interpretation (grouping, multi-artist credit splitting, cover choice). A library therefore looks the same on Mac, iPhone and TV.
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
   - **The server sends:** files, folders, sizes, dates, audio properties, tags as read, and for video the container's stream facts and tags (§5).
   - **The clients keep:** grouping into albums, artists and shows, cover ranking, filename parsing, TheTVDB matching, and labels such as "4K", "Dolby Vision" or "5.1". These stay in the apps' existing Swift code.
   - **Why:** the server stays small and stable, and the logic isn't duplicated in two languages.
2. **Replica first.** Every client keeps a local copy, refreshed by downloading the full index when it changes. There are no browse, search, paging or delta endpoints. This is how MiniMoon's iOS sync already works against `/files`.
3. **Read-only.**
   - **No client writes:** no route uploads, edits, renames or deletes anything, or stores client data. Play counts, ratings, resume positions, track choices and Fix Match stay in each app's own on-device store.
   - **Nothing written into library folders:** the server writes only its own index, caches and settings, in its app-data directory. No tag edits, sidecars, thumbnails or hidden files next to the media.
   - **The one exception, when pairing is on (1.1):** pairing (§4) records a paired device in the server's own settings. That's the server's configuration, not the shared folders.
4. **Never lose data on a bad read.** A scan that fails, a drive that's missing or a slice the server can't understand must never look like deleted files to a client (§5).
5. **One major version per path, additive changes within it.** Optional parts are listed in `features`. Clients ignore unknown fields and unknown enum values.
6. **Direct play only.** The server sends original bytes with Range support. Decoding is the client's job (AVFoundation in MiniMoon, VLCKit in Matinee).

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
- **Client identification:** requests to the JSON routes (`/v1/server`, `/v1/slices/…`, `/v1/events`, pairing) carry `MMP-Client: <app>/<version> (<platform>)`, e.g. `matinee/1.3 (tvOS 26.1)`. Media routes (`content`, `art`, `lyrics` and signed URLs) don't require it, because players like VLCKit and AVPlayer open those URLs themselves and won't send it.
- **Too-old clients:** the server may answer `400` with `error.code = "client_too_old"` for a client version known to be broken. (Not `426`: RFC 9110 §15.5.22 requires an `Upgrade` header naming a protocol to switch to, which doesn't fit "update your app".)
- **Errors:** always `{"error": {"code": "<stable_snake_case>", "message": "<English, for logs>"}}` with a suitable HTTP status. Clients localize from `code`. v1 codes:
  - `not_paired`, `token_revoked`, `url_expired` (pairing, 1.1)
  - `pairing_expired`, `pairing_code_wrong`, `pairing_locked` (1.1)
  - `not_found`, `range_not_satisfiable`, `method_not_allowed`
  - `folder_unavailable`, `slice_unavailable`
  - `client_too_old`, `unsupported_feature`

## 4. Access: open by default, optional pairing code (1.1)

There is one household and no user accounts. Every client that can reach the server sees every published slice.

**Open (`auth: none`, the default, and the only mode in 1.0).** No token, code or password. Any device on the local network can list the published slices and read the files in them, exactly as with the legacy Sync server today. Clients skip the pairing screen, and media URLs are plain `/v1/items/{id}/content` URLs that VLCKit and AVPlayer can open directly.

**Pairing (`auth: pairing`, protocol 1.1, a desktop-app setting: "Require a code for new devices").** A code rather than a password: the server shows six digits and the client asks for them once. There's nothing to remember or retype on each device, and still no account.

1. `POST /v1/pair` with `{"clientName": "Living Room", "app": "matinee", "platform": "tvOS"}`.
   - **Response:** `{"pairingId": "...", "expiresIn": 300}`.
   - **Server side:** the server shows a 6-digit code. The desktop app shows it in its window and a notification; the CLI prints it.
2. The user types the code on the client: `POST /v1/pair/{pairingId}/confirm` with `{"code": "123456"}`.
   - **Response:** `{"clientId": "...", "token": "<256-bit random, base64url>", "serverId": "..."}`.
   - **Limits:** 5 wrong codes lock that pairing. Pairing attempts are rate-limited per source address.
3. All other `/v1` requests send `Authorization: Bearer <token>`.
   - **Storage:** tokens are stored hashed in the server's settings, and in the Keychain on the client.
   - **Management:** the desktop app lists paired devices and can revoke any of them. A revoked token gets `401 token_revoked`.
   - **Revoking closes open downloads.** Checking each request only stops the next one, so on revocation the server also closes every content response it's streaming to that device. The server knows which device each response belongs to from its token or signed URL.
4. **Signed media URLs**, for players that can't set headers (VLCKit, AVPlayer): `GET /v1/items/{id}/content-url` returns `{"url": "/v1/c/{id}?client=...&gen=...&exp=...&sig=...", "expiresIn": 43200}`.
   - **Signature:** `sig` is an HMAC, made with a server key, over the item id, the client id, that device's current generation number and the expiry. Each paired device has a generation number in the server's settings, bumped when the device is revoked or re-paired.
   - **Checked on every request,** including each range request a player makes while seeking: the signature, the expiry, that the device is still paired, and that the generation matches. So a revoked device's URLs fail on their next request, not after the 12-hour expiry, and its open downloads are closed (step 3).
   - **Scope:** the URL grants that item's bytes, whatever version is current; it isn't tied to one version of the file. A file that changes during playback is handled as in §6, which is best effort.
   - **Renewal:** a request after expiry gets `401 url_expired`. The client asks for a new URL and hands it to the player, which covers seeking after a long pause.
   - **Slices still apply:** a signed URL for an item that's no longer in any published slice gets `404`, like every other route (§8).

**Switching modes.** Turning pairing on makes connected clients get `401 not_paired`; each shows the pairing screen once. It also turns off the legacy port (41324), which has no authentication; the desktop app says so when the setting is switched on. Turning pairing off makes the server ignore tokens and turns the legacy port back on.

`GET /v1/server` never needs a token; it's what a client calls first:

```json
{
  "id": "6f1c…", "name": "Living Room Mac mini",
  "auth": "none",
  "protocol": {"major": 1, "minor": 0, "features": ["art-resize", "events"]},
  "software": {"name": "minimoon-media-server", "version": "5.0.0"},
  "sharedFolders": [{"id": "a41f…", "name": "Music", "available": true},
                    {"id": "0c9e…", "name": "Movies", "available": false}],
  "slices":    [{"id": "6b0e…", "name": "Everything", "kind": "mixed", "available": true,
                 "etag": "\"9f2c…\"", "filesEtag": "\"18d4…\""},
                {"id": "2f9a…", "name": "Road Trip", "kind": "music", "available": true,
                 "etag": "\"41ab…\"", "filesEtag": "\"e07b…\""},
                {"id": "c71d…", "name": "Kids", "kind": "mixed", "available": false}]
}
```

- **One tree.** All shared folders (`sharedFolders`) are merged into a single tree, each appearing as a top-level folder. Each has a permanent `id` and a display `name`, which is only a label (§5, §8).
- **Slices** are named subsets of that tree, defined in the desktop app (§8). In v1 a slice is a selection of folders and files, like the Sync app's presets: one can take `Music/Jazz` from one disk and `Movies/Concerts` from another. Rule-based slices can come later without a protocol change, because clients never see how a slice is defined, only its files. Only slices switched to "published" in the desktop app are listed.
- **Each device chooses its slice.** The choice is made in the client app and stored on that device, never on the server, so changing it on one device doesn't affect any other. This is what lets a MiniMoon user give each device its own set of files: "Everything" on the living-room Apple TV, "Road Trip" on a phone with little storage, "Kids" on a child's iPad. A client may follow more than one slice.
- **The choice stays hidden until there is one.** When the server publishes a single slice (typically "Everything", the whole tree), the client follows it without asking and shows no slice picker anywhere. When a second slice appears, the device keeps following what it already had, and the picker appears in the app's server settings; there is no prompt. Devices remember slices by `id`, so renaming one changes nothing for them.
- **A followed slice disappears.** If a slice a device follows is no longer listed (deleted or unpublished in the desktop app), the device keeps its copy of that slice's files, shows them as offline, and asks what to follow instead. It never switches to another slice by itself, because a phone falling back from "Road Trip" to "Everything" could start a very large download.
- **`available: false` on a slice** means the server lists it but can't serve it right now: the definition was made by a newer server version and this one can't read it (§8). Its `/index` answers `503 slice_unavailable` and no content is served through it. Clients keep their copy and show it as offline, exactly as for a missing folder; they never treat it as deleted.
- **Following several slices.** The client keeps each followed slice's last accepted index separately and shows their union, merged by item `id`. A slice whose refresh fails keeps its previous index, so its files don't drop out of the union.
  - **One fetch at a time.** A client fetches one server's indexes one after another, never overlapping. When the same item appears in two indexes, the metadata from the later fetch wins, and a `304` counts as a fetch at the moment it arrives. Because fetches don't overlap, a later fetch always reflects the server's later state. Overlapping fetches could finish out of order and let older metadata overwrite newer, and an opaque ETag can't say which of two indexes is newer.
- **`kind`** is a hint for which app shows a slice (`music`, `video`, `mixed`). Items carry their own `kind`.
- **`sharedFolders[].available`** is false when that shared folder is unavailable (§5). The slices stay listed; only the files under that folder go offline.
- **`etag`** and **`filesEtag`** are the slice's current ETags for the full index and the file list (§5), so a client polling `/v1/server` can tell which slices changed without another request.
- **Pairing mode:** an unpaired client gets the same response with `sharedFolders` and `slices` omitted.

## 5. Slice sync: the full index, with an ETag

`GET /v1/slices/{sliceId}/index`, or `GET /v1/slices/{sliceId}/index?view=files` for the file list (below)

- **Response:** every item in the slice, and every folder needed to reach them, in one gzipped JSON response with an `ETag`:

```json
{
  "folders": [ … ],
  "items":   [ … ],
  "offline": ["0c9e…"]
}
```

- **`folders`:** every ancestor folder of every item, up to and including the shared folder itself, so a client can build the tree without gaps.
- **`offline`:** the ids of shared folders that are currently unavailable. Their items are listed from the server's last good scan when it has one. After the index is rebuilt it has none, and the folder has no items in the response (see "No previous snapshot" below); the client rule for offline folders keeps the client's copy either way.
- **Unchanged slice:** the client sends `If-None-Match: <etag>`; the server answers `304 Not Modified` with no body.
- **ETag:** changes whenever anything in the slice's index changes: a file added, removed or changed, metadata re-read, a folder going offline or coming back, or the slice's definition being edited in the desktop app. The server derives it from its index (for example a hash, or a counter it bumps on every change); clients treat it as opaque.
- **Client update:**
  - **Apply all or nothing.** The client applies an index only after it has downloaded and parsed it completely, and applies it in one transaction. A truncated download or a parse error leaves the previous copy untouched.
  - **Compare by item `id`,** scoped to the server's `id`. Items in the local copy but not in the index were removed, including files taken out of the slice. Items new in the index were added. The client's own state key (below) is used only to look up its on-device state, never to decide which files exist, so two files that share a state key stay two files.
  - **Offline folders lose nothing.** For a shared folder listed in `offline`, the client adds and updates items from the response but never removes any. That holds whether the server sends its last snapshot or, after an index rebuild, no items for that folder at all. Removals under that folder, including files taken out of the slice, are applied once it's back online.
  - **Metadata always comes from the newest index.** The client replaces every item's metadata (tags, audio, video facts) from each index it accepts, whether or not the file's size or mtime changed, so metadata corrected on the server reaches clients without any media being downloaded again.
  - **Media is downloaded again only when `size`, `mtime` or `rev` changed** (`rev` is defined in the wire schema below).
  - This is MiniMoon's iOS sync today (`SyncService.preparePlan`), plus the metadata rule. Run the comparison off the main actor: on a large library it once blocked the main actor for seconds (MINIMOON-B9).
- **The file list (`?view=files`).** A much smaller index for clients that copy the files and read their tags themselves, as MiniMoon's iOS app does today: it downloads changed files and rescans them, so it has no use for the server's metadata.
  - **`view`** takes exactly two values: `full` (the default when absent) and `files`. Any other value gets `400 unsupported_feature`. It's not a field list, so there's nothing to combine or parse.
  - **Response:**

```json
{
  "items": [
    {"id": "i42", "sharedFolderId": "a41f…",
     "path": "Miles Davis/Kind of Blue (1959)/01 So What.flac",
     "size": 41234567, "mtime": 1710000000000, "rev": 0}
  ],
  "offline": ["0c9e…"]
}
```

  - **Each item** carries only the required `id`, `sharedFolderId`, `path`, `size`, `mtime` and `rev`, all as defined in the wire schema below. `size` is included because the iOS sync compares size as well as mtime (`SyncService.preparePlan`), and `rev` so that a change the server detected with size and mtime unchanged still reaches the iOS app's copy. There are no folders, metadata or `kind`. A client builds folders from paths, and reads kinds and tags from the files it downloads.
  - **`offline`** is kept. A client must still not treat a missing shared folder's files as deleted.
  - **Its own ETag (`filesEtag`).** The file list is a different representation from the full index, so it has its own strong ETag. That ETag changes only when the list changes: a file added, removed, moved, or changed in size, mtime or `rev`, a folder going offline or coming back, or the slice definition changing which files it covers. Metadata re-reads that don't bump `rev` don't change it, so a server-side reader upgrade doesn't make the iOS app download the list again. `If-None-Match` and `304` work as for the full index.
  - **The client rules are the same,** apart from metadata: apply the list all-or-nothing, compare by item `id`, never remove offline folders' items, and redownload media when `size`, `mtime` or `rev` changed. The iOS app stores the `rev` it downloaded alongside size and mtime. The metadata rule doesn't apply, because the client reads metadata from its own copies of the files.
  - **Size (estimated):** about 180 bytes per item before gzip (the example above), roughly a quarter of a full audio item, so about 7 MB for 40,000 tracks, much less gzipped. That's comparable to the legacy `/files` listing the iOS app downloads today.
- **No deltas, cursors or tombstones.** Removals, moves, folder deletes and files added then removed between syncs all fall out of the client's comparison. The server keeps no change history.
- **Scans never publish a guess.** A removal reaches clients only when the server has positively seen that a file is gone:
  - **Unavailable shared folders.** When a folder is added, the server records the identity of the volume it lives on (its device ID or volume UUID). Before every scan it checks that the folder exists, is readable, and is still on that volume. An unplugged drive or unmounted network volume usually leaves an empty mount point behind on the startup disk, so the volume check, not emptiness, is what detects it. If the check fails, the folder is marked `available: false`, its last snapshot keeps being served in slice indexes (listed in `offline`), and its files' content, art and lyrics answer `503 folder_unavailable`.
  - **A scan error publishes nothing.** A scan error means failing to list a folder or to read a file's size and modification date: a permission error, an I/O error, a volume dropping partway through. Failing to read a file's metadata is not a scan error; that file is still listed with its required fields (§8, reading metadata safely). If a scan error happens under a shared folder, the server keeps that folder's previous snapshot whole and retries on the next scan. After repeated failures the desktop app shows which subfolders can't be read and offers to publish without them. That's an explicit choice by the user, never a silent partial publish. (Keeping only the failed subfolders' previous contents is a possible refinement later; v1 keeps the whole folder.)
  - **A scan that completes is the truth,** including a folder that's really empty: if it completes without errors, its removals are published.
  - **One step.** A completed scan replaces the folder's snapshot in a single database transaction, and slice ETags are computed after it. A client never sees half a scan.
  - **No previous snapshot.** On the first scan of a newly added folder, or after the server's index has been rebuilt from scratch, the folder is published only once a scan completes without errors. Until then it's listed as unavailable, not partial, and listed in `offline` with no items. That matters after a rebuild, when clients still hold copies the server no longer knows about: the client rule for offline folders (above) keeps them. A rebuild loses only the index; shared folders, their UUIDs and volume identities live in the server's settings (§8), so they survive it.
- **Slice removed or unpublished in the desktop app:** it drops out of `/v1/server`, and `/index` answers `404 not_found`. Clients handle it as in §4: keep their copy as offline and ask.
- **Size (estimated, not measured):** the example audio item below is about 650 bytes of JSON, so 40,000 tracks is roughly 25 MB, a few MB gzipped. A video item is somewhat larger because of its streams, but video libraries hold far fewer files. Parsing and comparing a 100,000-track library on an Apple TV should be measured before relying on it. If it proves too slow, a delta endpoint can be added later as an optional feature, since v1 changes are additive.

**Wire schema**

- **Conventions:**
  - **Absent means unknown.** A field the server couldn't determine is left out, never sent as `null` or guessed. Fields marked required below are always present.
  - **Units:** `size` in bytes; `mtime` in milliseconds since the Unix epoch (UTC); durations in milliseconds; sample rates in Hz; bitrates in kbit/s.
  - **Paths:** a `path` is relative to its shared folder and written with `/` between components. File names come from the file system as UTF-8, without normalization, and clients compare them exactly. A shared folder's own `path` is `""`. File names that aren't valid UTF-8 are skipped and logged. (`/` can't occur inside a file name on macOS, Windows or Linux, so the separator is unambiguous.)
- **Folder:** required `id`, `sharedFolderId` (the shared folder it belongs to), `name`, `path`; `parentId` on every folder except a shared folder itself.

  `{"id": "f3", "sharedFolderId": "a41f…", "parentId": "f1", "name": "Kind of Blue (1959)", "path": "Miles Davis/Kind of Blue (1959)"}`

- **Item:** required `id`, `sharedFolderId`, `parentId` (the folder containing it), `name`, `path`, `kind`, `size`, `mtime`, `rev`. `mime`, `audio`, `video` and `tags` appear when known.
- **`rev`** (integer, starting at 0) is a content revision for changes that size and mtime miss.
  - **When it's bumped:** when the server re-reads a file whose size and mtime haven't changed, with the same reader version as before, and gets different tags, embedded art (compared by a hash of the picture bytes) or audio or video facts. The bytes must have changed in place with the mtime kept, as some taggers can do.
  - **When it isn't:** a reader-version upgrade gives different results without the file changing, so a re-read by a newer reader never bumps `rev`.
  - **What it drives:** it's part of the content ETag (§6), so downloads, `If-Range` and the art cache all see the change, and clients redownload media when it changes.
  - **Limit:** an in-place change to audio or video data alone, with size and mtime kept and nothing the reader sees changed, still isn't detected.
  - **Rebuilds:** `rev` lives in the index, so a rebuild resets it to 0. A file whose `rev` was bumped then looks changed and is downloaded again, which is the safe direction.

```json
{
  "id": "i42",
  "sharedFolderId": "a41f…",
  "parentId": "f3",
  "name": "01 So What.flac",
  "path": "Miles Davis/Kind of Blue (1959)/01 So What.flac",
  "kind": "audio",
  "mime": "audio/flac",
  "size": 41234567,
  "mtime": 1710000000000,
  "rev": 0,
  "audio": {"durationMs": 562000, "sampleRate": 96000, "bitDepth": 24,
            "bitrate": 2304, "channels": 2},
  "tags": { "title": "So What", "artist": "Miles Davis", "albumArtist": "Miles Davis",
            "album": "Kind of Blue", "trackNumber": 1, "trackNumberTotal": 5,
            "diskNumber": 1, "year": 1959, "genre": "Jazz",
            "compilation": false,
            "replaygainTrackGain": "-6.20 dB", "replaygainAlbumGain": "-7.01 dB",
            "hasLyrics": false, "hasEmbeddedArt": true }
}
```

- **Item kinds:** `audio`, `video`, `image`, `subtitle`, `lyrics`, `other`. Sidecars (`cover.jpg`, `.lrc`, `.srt`) are ordinary items, and clients pair them.
- **Audio `tags`** are MMP's own schema, not a serialization of a Rust struct, so a change to the tag-reading code doesn't change the protocol. Each v1 field comes from one field of the copied tag reader (§8); `hasLyrics` and `hasEmbeddedArt` say whether it found lyrics and a picture:
  - **Strings:** `title`, `artist`, `albumArtist`, `album`, `genre`, `work`, `sortArtist`, `sortAlbumArtist`, `sortAlbum`, `replaygainTrackGain`, `replaygainAlbumGain`.
  - **Non-negative integers:** `trackNumber`, `trackNumberTotal`, `diskNumber`, `diskNumberTotal`, `year`, `dateYear`, `rating`.
  - **Booleans:** `compilation`, `hasLyrics`, `hasEmbeddedArt`. These three are always present in `tags` when `tags` is.
  - **Values are as read.** Text exactly as the file holds it (trimmed, as the reader does), nothing split or normalized; numbers as numbers; ReplayGain as the file's own text, such as `"-6.20 dB"`.
  - **`year` and `dateYear`.** The reader has two sources: `year` is the file's year field, and `dateYear` is the year of its recording-date field. Both are sent when present, and clients choose between them as the apps do today.
  - **`rating`** is the number the file holds, on that format's own scale: 0–100 for an MP4 `rate` atom, 0–255 for a Vorbis comment `RATING` (as MiniMoon writes it). The scale for ID3 `POPM` as the reader reports it is to be confirmed by the comparison test (§11). Clients know the format from `mime` and keep their own ratings locally.
  - **No `movement` in v1.** The tag reader has no movement field. It can be added, with the reader, as an additive change.
  - **Lyrics** text is fetched separately (§6).
  - **Mapping:** the server maps its tag reader's fields onto this schema. Adding a field is an additive change.
- **Video item:** the container's stream facts and tags:

```json
{
  "id": "i97", "sharedFolderId": "0c9e…", "parentId": "f20", "name": "Show.S02E05.2160p.mkv",
  "path": "Show/Season 2/Show.S02E05.2160p.mkv",
  "kind": "video", "mime": "video/x-matroska", "size": 18734002176, "mtime": 1712000000000, "rev": 0,
  "video": {
    "durationMs": 3134000, "container": "matroska",
    "streams": [
      {"type": "video", "trackNumber": 1, "codec": "hevc", "width": 3840, "height": 2160,
       "frameRate": 23.976, "transfer": "pq", "dolbyVisionProfile": 8},
      {"type": "audio", "trackNumber": 2, "codec": "eac3", "channels": 6, "language": "eng",
       "title": "Surround 5.1", "default": true},
      {"type": "subtitle", "trackNumber": 3, "codec": "subrip", "language": "eng",
       "default": false, "forced": false}
    ]
  },
  "tags": {"title": "…", "show": "…", "season": 2, "episode": 5, "year": 2019}
}
```

  - **`streams`:** every track, in container order.
    - **`trackNumber`** is the container's own track identifier (Matroska `TrackNumber`, MP4 `track_ID`). Clients use it to match a stream to the player's tracks; container order alone isn't safe, because players can skip or renumber tracks. How each player's track ids map to `trackNumber` has to be confirmed per player (§11).
    - **Video streams** give codec, size, frame rate and colour facts: `transfer` (`sdr`, `pq` or `hlg`), `dolbyVisionProfile` (a number such as `5` or `8`, present only when Dolby Vision is detected) and `hdr10Plus` (`true` only when detected).
    - **Audio streams** give codec and channels; **subtitle streams** give codec.
    - **Every stream** gives language, title and default/forced flags where the container has them.
  - **Detected versus absent.** An absent `dolbyVisionProfile` or `hdr10Plus` means "not detected", not "confirmed absent". HDR10+ in particular is checked in the first frame only. Clients treat absence as unknown.
  - **Reading rules** that the reader test showed are needed:
    - **Colour comes from the codec headers.** `transfer` is read from the HEVC or H.264 SPS colour fields (VUI) or the AV1 sequence header. Matroska `Colour` elements are used only if present; none of 106 real MKVs had them.
    - **Dolby Vision** comes from the configuration record (Matroska `BlockAdditionMapping` or MP4 `dvcC`/`dvvC`). Profile 5 implies PQ even though its base layer usually carries no transfer value.
    - **HDR10+** is only in SEI messages in the frames, so the reader checks the first HEVC frame.
    - **`frameRate`** is the nominal rate: Matroska `DefaultDuration`, or MP4 sample durations, falling back to timing the first frames. Variable-rate files report their nominal rate.
    - **`channels`** is the decoded channel count. HE-AAC with parametric stereo is stereo even though the container says 1.
    - **`language`** is BCP 47 when the container has it, otherwise its ISO 639-2 code. An absent Matroska Language means `eng`, per the Matroska spec.
  - **Video `tags`:** what the container itself holds: `title`, `show`, `season`, `episode`, `year` and `description`. For MP4 those come from the iTunes-style atoms; for Matroska, from its title and tags. They're hints for Matinee's filename parsing and TheTVDB matching, not a replacement for either.
  - **Matinee's mapping:** `video` covers everything in Matinee's `ProbeResult`:
    - duration, size, video codec and frame rate;
    - dynamic range, from `transfer`, `dolbyVisionProfile` and `hdr10Plus`;
    - the first audio stream's codec and channels;
    - audio and subtitle stream counts.
    Matinee fills `ProbeResult` from the index and skips its own VLCKit probe. That probe costs a network parse of up to 10 seconds per share file today, repeated after every tvOS cache purge. Per-track language and default/forced flags, together with `trackNumber`, also let it apply audio and subtitle language preferences before playback.
  - **Unreadable files:** a file whose metadata can't be read is still listed and served, with only the required fields. Matinee falls back to probing it with VLCKit as today.
- **Identity:**
  - **`id`** is a hash of the shared folder's `id` and the item's `path`. It survives server index rebuilds, is the same in every slice, and changes only when the file is moved or renamed on disk. Renaming a shared folder's display name changes nothing. Used in content, art and lyrics URLs.
  - **Clients key their on-device state by facts they already use, scoped to the server's `id`,** so two servers with the same paths never collide:
    - **MiniMoon:** (server `id`, shared folder `id`, `path`), its path identity today with the folder and server added. A file moved on disk loses its state, as today.
    - **Matinee:** (server `id`, `name` + `size`), the key it already gives share files (`ShareKeys.ownKey`), so its existing share machinery applies. A file moved to another folder keeps its state; a renamed one doesn't. Two different files with the same name and size share state; that's Matinee's existing trade-off for share files, accepted rather than fixed. They're still two catalog entries, because which files exist is decided by item `id` (§5, client update).
  - **No content fingerprint in v1.** A hash over the start of a file changes whenever a music file is retagged, because ID3 and FLAC tags sit at the front. Neither app needs one for server files. It can return as an optional feature if deduplication ever needs it.
- **Push:** with the `events` feature, `GET /v1/events` is a Server-Sent Events stream sending `slice-changed {sliceId, etag, filesEtag}`, `folder-availability {sharedFolderId, available}`, and `slices-changed` when a slice is added, removed, renamed, published or unpublished. Events are hints only. Clients act on them by re-checking `/v1/server` and `/index` with `If-None-Match`, and do the same after every reconnect, because events sent while disconnected are lost. Clients that don't use events re-check when they come to the foreground.

## 6. Content, artwork, lyrics

- **File bytes:** `GET /v1/items/{id}/content`, or in pairing mode the signed `/v1/c/{id}?…` (§4).
- **HTTP contract for content:**
  - **Headers:** `Accept-Ranges: bytes`, `Content-Length`, the real `Content-Type`, `Last-Modified`, and a strong `ETag`.
  - **ETag:** derived from the file's size, its mtime at full precision and its `rev` (`"<size>-<mtime in ns>-<rev>"`, §5). It's declared strong (RFC 9110 §8.8.1) on the assumption that any change to a file's bytes changes its size, its mtime or its `rev`. That assumption breaks only when something rewrites a file in place, keeps its size, puts the old mtime back, and changes nothing the reader sees, such as audio data alone. A strong ETag is needed because RFC 9110 §13.1.5 forbids clients from sending a weak one in `If-Range`.
  - **Ranges:** a single range per request, in any of the three forms (`bytes=a-b`, `bytes=a-`, `bytes=-n`), answered with `206` and `Content-Range`. A request for several ranges gets the whole file with `200`, which RFC 9110 allows (§14.2). A range starting past the end gets `416` with `Content-Range: bytes */<size>`.
  - **Conditional requests:** `If-Range` with the ETag (a stale ETag gets the whole file with `200`), and `If-None-Match` (`304`).
  - **HEAD** returns the same headers as GET, with no body.
  - **Files that change mid-transfer (best effort).** With `Content-Length`, a response is complete as soon as its last byte arrives (RFC 9112 §6.3), so closing the connection afterwards can't make the client reject it. The check therefore comes before the last byte:
    - the server records the file's size and mtime when a response starts;
    - after reading the response's final chunk from disk, and before sending it, it checks them again;
    - if either changed, it closes the connection without sending that chunk, so the client sees an incomplete transfer, not a silently mixed file.
    Any write that landed before the final read changed the mtime by then, so on a local disk this catches every in-place change that affected the bytes sent. On network volumes the host mounts, attribute caching can hide a change, so there it's best effort. Apps that save by writing a new file and renaming it over the old one are safe anyway on macOS and Linux, because the server keeps reading the file it opened.
  - **Across range requests.** A player seeking through a file makes many separate requests. Each response carries the ETag, but a changed file is caught across requests only if the player sends `If-Range`, or compares the ETag itself. Whether VLCKit and AVPlayer do is unverified and is part of the playback test (§10). Until that's known, consistency across a playback session is best effort.
  - **Interrupted downloads** resume with `Range` plus `If-Range`:
    - **`206`:** the client checks that `Content-Range` starts exactly at the length of its partial file, appends the body, and checks the finished file's length against the total in `Content-Range` (the `/<size>` part). `Content-Length` on a `206` is the length of the range, not of the file.
    - **`200`:** the file changed in the meantime, or the server ignored the range. The client discards its partial copy and writes the body from the start, checking it against `Content-Length`.
  - **MiniMoon:** downloads to its cache, checks the finished file's length as above, then opens it with `AVAudioFile`.
  - **Matinee:** hands VLCKit the URL and streams directly. Whether VLCKit's HTTP access plays, seeks and recovers correctly against this server is the first thing to test once a prototype server exists (§10).
- **Embedded artwork:** `GET /v1/items/{id}/art` returns the first embedded picture, or `404`. For video, that's a Matroska cover attachment or the MP4 `covr` atom. Same headers and conditional-request rules as content.
- **Resized images** (`art-resize` feature): `?maxSize=600` scales either an item's embedded art or an `image` item's content to fit within 600 pixels, as JPEG. Results are cached in the server's app-data directory, never next to the media. The cache key is the item `id`, its content ETag, the requested `maxSize` and the resizer's version number, so a changed source, a different size or an improved resizer each produce a new entry.
- **Embedded lyrics:** `GET /v1/items/{id}/lyrics` returns `text/plain`.

## 7. Client-side state (not part of the protocol)

The server stores nothing on behalf of clients. Each app keeps its state where it does today:
- **Matinee:** `PersistentRecord` in UserDefaults on the Apple TV (400 KB budget). A share file gets a record entry only once it has state worth keeping (watched, a position, a Fix Match or rename, a track choice); its automatic match lives in a purgeable cache sidecar. MMP files would follow the same rule.
- **MiniMoon:** its own library database on each device.
- **Syncing state between devices** is a separate decision outside MMP. Matinee's candidate is iCloud (`docs/monetization.md`).

## 8. Server behaviour

- **Shared folders and slices:**
  - **Shared folders** are the folders on disk the user shares, added in the desktop app or with CLI flags. The Sync app has one today; MMP allows several. They're merged into one tree, each as a top-level folder.
  - **Identity versus label.** Each shared folder gets a permanent UUID when it's added. Identity (§5), selections and availability all use that UUID. Its display name defaults to the folder's name on disk; a clash gets a number added (`Music 2`). The user can rename it freely, since renaming changes only the label.
  - **Slices** are named subsets of that tree. In v1 every slice is a selection, edited in a file-selection tree that shows every shared folder at the top and works at any depth, as the Sync app's tree already does. There is no "active" slice.
  - **Published.** Each slice has a "published" switch in the desktop app. Only published slices are listed in `/v1/server` and served. A slice can be prepared before devices see it, and old presets don't appear on every device after migration.
  - **Selections** pick whole folders, with everything under them, and single files, at any depth. A whole shared folder is selected by selecting its top-level folder.
  - **Companion files.** Selecting a single file also selects its companions, and the selection UI shows them (e.g. "+2 subtitle files"), so nothing is broadened silently. Only three kinds of file can be companions, matched by extension, case-insensitively:
    - **Subtitles** (`srt`, `ass`, `ssa`, `vtt`, `sup`, and `idx` with `sub`), whose name is the selected file's stem plus any further `.`-separated parts: `Movie.srt`, `Movie.en.forced.srt`, `Movie.idx` with `Movie.sub` for `Movie.mkv`.
    - **Lyrics** (`lrc`), named the same way: `song.lrc` for `song.flac`.
    - **The folder's cover image:** `cover`, `folder` or `front` with `jpg`, `jpeg`, `png` or `webp`, whenever any file in that folder is selected.
    - **Rules:**
      - Companions are looked for only in the selected file's own folder, never in subfolders, and only for files the user selected directly; a companion's own companions aren't added.
      - Any other file sharing the stem, such as `Movie.notes.txt`, `Movie.backup.zip` or `Movie.original.mkv`, is not a companion.
      - A selection is evaluated at every scan, so a companion added later (subtitles downloaded after the movie, say) is published automatically.
    Selecting a whole folder already includes all of these and everything else in it.
  - **`kind`** is chosen per slice in the desktop app, defaulting to the kind most of its files are.
- **Legacy sync:**
  - **What it serves.** Port 41324 still serves the one folder the Sync app shared, with paths relative to it as today. A desktop-app setting picks which slice it applies, limited to that folder, defaulting to the preset that was active at the upgrade. If the chosen slice isn't a `selection`, legacy sync uses the files it resolves to within that folder.
  - **It's on only while pairing is off.** It has no authentication, so it would let any device bypass the pairing code. Turning pairing on turns it off (§4).
  - **Pause covers both ports.**
  - Otherwise its routes stay as they are, unfixed, because they're barely used now. They can be removed once older clients are gone.
- **Slice definitions (server persistence, not part of the wire protocol).** Slices are stored in the server's settings, not in the Sync app's web storage (`localStorage`, in `state.ts`), because the server keeps running when the window is closed and the CLI needs them too. Clients never see this format.
  - **A typed syntax tree, not a query language.** A definition is a tree of typed nodes, and each node type has a fixed set of typed fields:
    - properties are named by closed enums, never by strings like `"tags.genre"`;
    - comparisons are typed per kind of value, never by operator strings;
    - paths are a shared folder UUID plus an array of components, never strings with separators or trailing-slash conventions.
    Nothing in the format is parsed out of a string, and the desktop app's editors build the tree directly.
  - **Normative definition** (Rust, as the server would declare it; JSON is its serde serialization, with node types tagged by `"type"` and names in camelCase):

```rust
struct SliceFile { format_version: u32, slices: Vec<Slice> }

struct Slice {
    id: Uuid,                 // created with the slice, never reused; the protocol's slice id
    name: String,
    kind: SliceKind,          // Music | Video | Mixed: the hint in /v1/server
    published: bool,
    definition: SliceExpr,
}

/// A path in the unified tree: a shared folder plus components within it.
struct TreePath { folder: Uuid, path: Vec<String> }

enum SliceExpr {
    // v1
    All,                                                 // every file in every shared folder
    Selection { folders: Vec<TreePath>, files: Vec<TreePath> },  // files bring their companions

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
    {"id": "6b0e…", "name": "Everything", "kind": "mixed", "published": true,
     "definition": {"type": "all"}},
    {"id": "2f9a…", "name": "Road Trip", "kind": "music", "published": true,
     "definition": {"type": "selection",
                    "folders": [{"folder": "a41f…", "path": ["Jazz"]},
                                {"folder": "a41f…", "path": ["Rock", "Classics"]}],
                    "files": [{"folder": "a41f…", "path": ["Misc", "song.flac"]}]}}
  ]
}
```

  - **A later rule-based definition** (illustrative; not v1): jazz from the 1950s and 60s in the Music folder:

```json
{"type": "and", "children": [
  {"type": "selection", "folders": [{"folder": "a41f…", "path": []}], "files": []},
  {"type": "text", "field": "genre", "test": {"type": "contains", "value": "Jazz"}, "caseSensitive": false},
  {"type": "number", "field": "year", "test": {"type": "between", "min": 1950, "max": 1969}}
]}
```

  - **Fields map to index facts, one each.** Every field variant names exactly one fact from §5:
    - `Genre` is the audio tag `genre`;
    - `Width` is the first video stream's width;
    - `Modified` is `mtime`.
    That mapping lives in the server's code, not in the file. A file whose fact is missing doesn't match a test on it.
  - **Extending** means adding variants: new node types, fields or tests. That doesn't change `formatVersion`, which only changes if an existing type's meaning changes.
  - **Strict parsing, fail closed, never vanish.**
    - **Each slice is parsed on its own.** Its `id`, `name`, `kind` and `published` are always read; its `definition` is parsed strictly, rejecting unknown variants and unknown fields.
    - **A definition that doesn't parse** (made by a newer server, say) means the slice is listed with `available: false` and serves nothing (§4). It's never published with a guess, and it never silently disappears, which clients would read as deletion.
    - **In the desktop app** such a slice shows read-only as needing a newer version.
    - **Writing back.** The server stores the slice verbatim and writes back only the slices it parsed, so an older server never drops or alters a newer one's definitions.
    - **A newer `formatVersion`** makes every slice in the file unavailable in the same way.
  - **Evaluation** happens on the server against its index. A slice's membership, and so its ETag, changes when its definition changes or when a file's facts change; for a future rule slice that includes a retag that moves a song in or out. Clients never see definitions, only `/v1/server` entries and indexes, so new node types need no client change.
  - **Migration from the Sync app.** On first launch after the upgrade, the app hands its existing presets to the server, which converts each to a slice:
    - **Paths.** The Sync app stores a preset as path strings in which a trailing `/` marks a folder. The migration is the one place that convention is read: entries ending in `/` become `folders`, the rest `files`, each split into components under the shared folder's UUID (`Jazz/` becomes `{"folder": "a41f…", "path": ["Jazz"]}`).
    - **Empty presets.** A preset with an empty selection becomes `all`, which also covers any folder shared later.
    - **Only the active preset is published.** The other presets are migrated unpublished, and the desktop app shows them as migrated, so the user decides which ones devices should see. Before the upgrade only the active preset was listed to devices. (Raw downloads from the legacy server already ignored the selection, so this is about what's listed, not new file access.)
- **Enforcement:** an item is served (`/content`, `/art`, `/lyrics`, signed URLs) only if it's in at least one published, available slice; otherwise it gets `404`, as if it didn't exist. Item routes aren't scoped by slice, because every client can see every published slice. Slices are enforced by the server on every route, not only in listings. (The legacy server applies its selection only to `/files`; that's left as is, since the legacy API is barely used.)
- **Index:**
  - **Contents:** a SQLite database in the app's data directory, holding files, folders, their metadata and `rev`, and each shared folder's last snapshot. Each file is indexed once, however many slices include it.
  - **Reader version:** each indexed file records the version of the metadata reader that read it.
  - **Settings, not index:** everything that can't be rebuilt from the disks lives in the server's settings, so rebuilding the index from scratch never changes an identity:
    - the server's `id`;
    - the shared folders: UUID, path on disk, display name and volume identity;
    - slice definitions, which refer to shared folders by UUID;
    - when pairing is on, paired devices and the URL-signing key.
  - **Why it exists:** so metadata isn't re-read on every request; the legacy `/files` walks the whole tree on every call.
  - **Ready responses:** the server may also keep each slice's gzipped index response ready, rebuilt when the slice changes.
- **Change detection:**
  - **Triggers:** file-system watching, plus a full rescan at startup and periodically.
  - **Re-reading metadata:**
    - files whose size or mtime changed are re-read;
    - files read by an older reader version are re-read in the background after a server update, so reader fixes reach existing files and then clients, through the metadata rule in §5, without any media being downloaded again;
    - a "Reindex everything" command in the desktop app re-reads every file. Where a file's size and mtime are unchanged but the same reader version now reads different tags, art or stream facts, its `rev` is bumped (§5), so clients download it again and resized art is regenerated. It doesn't catch an in-place change the reader can't see, such as audio data alone.
  - **Scan safety:** the volume check, scans that publish nothing after a scan error, and one-step publication are as in §5.
- **Tag reading: a copy, not a dependency.**
  - **What's copied.** The server doesn't depend on the apps' `minimoon_tags` crate. It carries a copy of only the reading side: `read_tag` and its helpers, the native-text and sort-tag reading, and embedded-picture reading. The crate's writing and tag-copying code, and its C interface for Swift, stay out.
  - **Why.** The crate is a thin layer over `lofty`, so the copy is small. The server avoids building the apps' static library and its Swift-facing interface, and it doesn't carry write code it must never call (§2, read-only).
  - **`lofty` version.** The crate pins `lofty` to a git revision for an ID3v2 write fix. The read-only copy can use a crates.io release, after checking that it reads a sample library the same as the pinned revision.
  - **Keeping them aligned.** Advantage 3 (§1) depends on the server and the apps reading tags the same way, and a copy can drift. A comparison test runs both readers over the same sample files and fails on any difference in the fields MMP sends. When the apps' reader changes, the copy is updated and its reader version (§8, index) is bumped, so existing files are re-read.
- **Reading metadata safely:**
  - a failure to read a file's metadata never stops the file being listed or served (§5);
  - the reader has a time and size limit per file, and a file that exceeds it is indexed with the required fields only;
  - errors are logged and shown as a count in the desktop app.
- **Sleep:**
  - **While streaming:** while any content response is streaming, the server holds a "prevent idle sleep" assertion on macOS (and the Windows equivalent).
  - **Grace period:** the assertion is kept for a grace period after the last request, because players buffer ahead and can go quiet for a while in the middle of playback. Its length needs tuning against real buffered playback in VLCKit and AVPlayer.
  - **Optional setting:** "Keep this computer awake while sharing".
  - **Clients** show a distinct "server asleep or off" state and keep browsing from their local copy.
- **Network visibility:** the desktop app shows "Sharing on ‹network›" and has a one-click pause covering both ports, because in open mode a laptop shares its slices with any network it joins (§9).

## 9. Security in v1, and what's deferred

- **Open in 1.0.** Anyone on the same network can read the published slices. That's the same exposure as the legacy Sync server, which has no auth.
  - **The real risk is a laptop that moves:** with the server running, a MacBook on hotel or café Wi-Fi advertises itself and serves its slices to everyone there.
  - **1.0 mitigation:** the network name, and the pause control covering both ports (§8).
  - **1.1:** the optional pairing code (§4), which also turns off the unauthenticated legacy port.
  - **Possible later:** remember the network chosen at setup and pause sharing on any other. Reading the Wi-Fi name on recent macOS may need Location permission (from memory, not checked).
- **Slices are not access control.** They decide what's published, not who can see it: every client can see every published slice. A "Kids" slice hides nothing from any other device. Files outside every published slice are never served.
- **Nothing to damage.** Because no route writes, an unwanted client can read the published files but can't change, delete or add anything, or alter another device's state.
- **Pairing mode (1.1):** a code shown on the server proves consent; every API call and media URL is authenticated, and revoking a device makes its next request fail and closes its open downloads (§4).
- **Plain HTTP.** Media, and in pairing mode the token, can be sniffed by anyone on the same LAN.
  - **Why plain:** TLS with a self-signed, pinned certificate works in URLSession through its challenge delegate, but it's unverified for VLCKit's HTTP access and for AVPlayer without a resource loader.
  - **Later:** a `tls` feature with the certificate fingerprint pinned at pairing, once VLCKit's behaviour is tested.
- **Inside chosen folders and published slices only.** The server never exposes paths outside the shared folders or outside every published slice, filters hidden files, rejects path traversal, and never follows symlinks out of the shared folders unless the user opts in. This matters more in open mode. The legacy `/file` route skips some of these filters, so it's not a model to copy.

## 10. Smallest first version

**Core (protocol 1.0)**

- **Server:**
  1. **A playback test first.** Build a minimal `/content` endpoint with the HTTP contract in §6. Before anything else depends on it, check that VLCKit (Matinee) and AVPlayer/URLSession (MiniMoon) play, seek, and recover from a dropped connection and a changed file, including whether each sends `If-Range` or checks the ETag across range requests.
  2. Rename and rebrand the server; leave the legacy routes in place. Keep the bundle identifier (`com.plastaq.sync-server`) and the updater endpoint so existing installs upgrade in place. Whether the Tauri updater handles a product-name change cleanly is unverified.
  3. `/v1/server` and Bonjour advertising, open mode only.
  4. **The index:**
     - shared folders (UUIDs, paths, names, volume identities) in the server's settings, and a SQLite index of files and metadata, with reader versions and `rev`;
     - the scan-safety rules in §5: the volume check, nothing published after a scan error, one-step publication, no partial first publish;
     - `/index` with ETag and `304`, in both views (`full` and `files`), each with its own ETag.
     This is the largest server piece.
  5. `/content` (from step 1), `/art` and `/lyrics`.
  6. **Desktop app UI:**
     - shared folders and their display names;
     - slices, using the existing selection tree spanning every shared folder, with companion files shown, and without the active-preset switch;
     - a published switch per slice;
     - the legacy-sync slice setting;
     - the sharing network and a pause covering both ports;
     - read-error and metadata-error reporting, and "Reindex everything".
     Slices are stored in the server's settings in the format in §8 (`all` and `selection` only), migrating existing presets with only the active one published.
  7. **Tag reading, copied in.** Copy the reading side of `minimoon_tags` into the server (§8), and extend the copy with duration in milliseconds (the crate reports whole seconds, `properties.duration().as_secs()`) and a channel count. The apps' crate doesn't change.
  8. **Video reading in native Rust**, chosen by the reader test ([video-reader-test.md](video-reader-test.md), 2026-11-14):
     - **Crates:** `matroska-demuxer`, `mp4-atom`, `scuffle-h265`, `h264-reader`, `scuffle-av1`, and `lofty` for MP4 tags.
     - **Hand-written parsing:** about 240 lines, covering the Dolby Vision configuration record, Matroska attachment headers, raw EBML and MP4 box scanning (`mp4-atom` drops `dvcC`/`dvvC`), and the HDR10+ SEI check.
     - **Results:** on 107 real files, it agreed with ffprobe and MediaInfo on duration, codec, size, transfer, Dolby Vision, HDR10+ and covers for every file. The tools differed on frame rate, channel count, language and title for some files, as listed in the report. On 10 synthetic HDR clips it detected every HDR10, HLG, HDR10+ and Dolby Vision 5 and 8.1 case, but didn't read the MP4 forced flag. It ran in a median of 19 ms and read a median of 100 KB per file. ffprobe read 5 MB per file and would need a custom LGPL build; MediaInfoLib is an 8 MB C++ library needing FFI. The test is not comprehensive format validation; its gaps are listed in the report and in §11.
     - **Still to do:** read MP4 forced and default flags and track titles; report HE-AAC parametric stereo as 2 channels; add `trackNumber`.
     - **Placement:** in the server, next to the copied tag reader.
- **Shared Swift client package:** used by both MiniMoon and Matinee, as a new Swift package with its own repo, since Matinee and MiniMoon are separate repos. It covers:
  - discovery (`NWBrowser`);
  - the per-device slice picker, with the choice stored on the device by slice `id`;
  - keeping one accepted index per followed slice and showing their union;
  - fetching `/index` in either view with `If-None-Match`, one fetch at a time per server, and applying it all-or-nothing in one transaction, off the main actor;
  - comparing by item `id`, with app state keys kept for state lookups only;
  - the metadata rule;
  - offline folders (never removing their items) and unavailable or disappeared slices, shown as offline, never as deleted;
  - content URLs with resumable downloads checked against `Content-Range`, and an error mapping.
- **MiniMoon:**
  - **iOS (copies files):** syncs from the file list (`?view=files`). It downloads added and changed files and rescans them locally, as it does today against `/files`. This replaces its legacy sync for MMP servers, with offline folders, resumable downloads and per-device slices added.
  - **macOS and tvOS (no local copy):** an MMP `LibrarySource` mapping items from the full index to `Track` rows, plus fetch-to-cache playback.
  - **Switching slice** on a device removes the files that are no longer in it from that device, through the normal comparison.
- **Matinee:** an MMP source feeding `LibraryStore` the way a share does, filling `ProbeResult` from each item's `video` facts instead of probing, and matching streams to VLCKit tracks by `trackNumber`. It plays content URLs through VLCKit. Its existing share machinery (record keys, the state-worth-keeping rule, offline state, the title cap) applies.

**Next (protocol 1.1, already designed above)**

- Optional pairing code, tokens and signed URLs bound to device and generation (server setting, client pairing screen, Keychain storage), including turning the legacy port off while pairing is on.
- `events` (SSE).
- `art-resize`.

**Later, separate decisions**

- TLS.
- Pausing on unrecognised networks.
- Keeping only failed subfolders' previous contents after a scan error, instead of the whole shared folder.
- Jellyfin and OpenSubsonic client backends in the apps.
- Chapters and more video facts (bit depth, bitrate per stream), added to `streams` as optional fields.

**Out of scope for MMP:** any write route, including state sync. Cross-device state, if wanted, goes through iCloud or another channel the apps own.

## 11. Verified vs unverified

**Verified**

- **Code:**
  - The `minimoon_tags` `Tag` and `Properties` fields: single-string artist, ReplayGain gains only, duration in whole seconds, no channel count.
  - `minimoon_tags` (`~/c/m/tagging`) is a thin layer over `lofty`. Its reading side is `read_tag` and its helpers in `tag.rs`, the reading helpers in `native_text.rs` and `sort.rs`, and `get_picture_from_path`. It also writes and copies tags, which the server doesn't need. It pins `lofty` to a git revision for an ID3v2 write fix (`Cargo.toml`).
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
  - The tag reader's fields (`tagging/src/tag.rs`):
    - it has no movement field;
    - it reads both a year field and a recording date, whose year it keeps;
    - `rating` is a raw number: an MP4 `rate` atom holds 0–100, and MiniMoon writes Vorbis `RATING` as 0–255 (`native_text.rs`).
- **Standards:**
  - RFC 9110 §15.5.22: a `426` response must carry an `Upgrade` header.
  - RFC 9110 §13.1.5: clients must not send a weak ETag in `If-Range`.
  - RFC 9110 §14.2: a server may ignore a request for several ranges.
  - RFC 9112 §6.3: with `Content-Length`, a message is incomplete only if the connection closes before that many bytes arrive, so closing afterwards doesn't make a complete response fail.
- **Apple and competitors:**
  - **Local network privacy:** tvOS doesn't have it (TN3179).
  - **Plex:** remote playback of personal video needs Plex Pass or a Remote Watch Pass since April 2025, enforced in stages. Local playback is unaffected.
  - **Jellyfin:** its setup wizard creates an admin account. The server is GPL-2.0.
  - **Swiftfin:** the Jellyfin tvOS client is video-only, per its 2022 announcement and a 2026 third-party guide. Its current release isn't checked.
  - **Subsonic:** clients use an empty `search3` query for a full sync.
  - **Home Sharing:** the Computers app is still documented for tvOS 26.

**Unverified**

- Whether Jellyfin or OpenSubsonic have a change feed.
- What users actually use Sync app presets for. Publishing only the active preset at migration keeps today's listing; anyone who switched presets to change what every device sees now does that per device instead.
- Index size and the time to parse and compare it on an Apple TV for a 100,000-track library (only estimated).
- VLCKit and AVPlayer playback, seeking and recovery against this server's HTTP contract (the first step in §10), including whether either sends `If-Range` or checks the ETag across range requests.
- The scale of an ID3 `POPM` rating as the tag reader reports it.
- Whether mtime checks see in-place changes promptly on network volumes the host mounts.
- How VLCKit's and AVPlayer's track ids map to the container's `trackNumber`.
- How long players go quiet between requests while buffering, which sets the sleep grace period.
- Whether volume identity (device ID or volume UUID) reliably detects an unmounted volume on macOS and Windows, including network volumes.
- VLCKit with self-signed TLS.
- The Tauri updater across a product-name change.
- The Music app's lack of FLAC support (from memory).
- The video reader on real Dolby Vision and HDR10+ releases (only synthetic clips were tested), on Dolby Vision profile 7, on iTunes MP4s, AVI, MPEG-TS and WebM, over network volumes, and on Windows.
- Whether reading the Wi-Fi name on macOS needs Location permission (from memory).
- Whether Bonjour Sleep Proxy wakes the host on access.
- Wake-on-LAN from tvOS.
- `notify`-crate reliability on network volumes the host itself mounts.
