# MiniMoon Media Protocol (MMP) v1: proposal

Status: revised 2026-11-15. The server side of protocols 1.0 and 1.1 is built in this repository (§10 lists what's done and what isn't). The shared client package and the apps' support aren't built.

- **Server:** minimoon-media-server, the renamed minimoon-sync.
- **Clients:** MiniMoon (macOS, iOS, tvOS) and Matinee (tvOS, macOS).
- **MMP only:** the server speaks only MMP. The Sync app's protocol (`/files`, `/file-by-path` on port 41324) is dropped, not kept alongside. The apps handle the move from it to MMP; that transition is outside this spec.
- **Read-only:** clients never write anything to the server or its folders. Every index and item route is a GET; the only POSTs are the pairing routes (protocol 1.1), which record a device in the server's own settings (§4). Watch progress, play counts, favourites and every other kind of per-user state stay on the client device, as they do today.

## 1. Why this exists: the advantage over Jellyfin, Navidrome and Plex

The project is worth doing only if it wins on something the incumbents can't easily copy.
Three claims hold up. Some common claims don't, and shouldn't be made.

**Real advantages**

1. **No administration.**
   - **MMP:** a desktop app with a folder picker, and every device sees everything in the chosen folders (§4). The server appears on the TV over Bonjour and works with no code, account or password. An optional pairing code comes in protocol 1.1 (§4).
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
4. **Never lose data on a bad read.** A scan that fails or a drive that's missing must never look like deleted files to a client (§5).
5. **One major version per path, additive changes within it.** Optional parts are listed in `features`. Clients ignore unknown fields and unknown enum values.
6. **Direct play only.** The server sends original bytes with Range support. Decoding is the client's job (AVFoundation in MiniMoon, VLCKit in Matinee).

## 3. Transport, discovery, versioning

- **Transport:** HTTP/1.1 (HTTP/2 allowed). JSON in UTF-8, gzip if the client accepts it.
- **Methods:** every `/v1` route is GET (and HEAD), except the two pairing routes in §4. The server answers any other method with `405`.
- **Port:** 41325 by default, not the Sync server's 41324, so a client never mistakes one protocol for the other.
- **Encryption:** plain HTTP in v1; see §9 for why and what replaces it later.
- **Bonjour:** the server advertises `_mmserver._tcp` (8 characters, under the 15-character limit). TXT records:
  - `id`: the server's UUID, persisted for the life of the install
  - `pv`: the protocol majors it supports, e.g. `1`
  - `name`: the display name
  - `auth`: `none` or `pairing`
- **Manual entry:** clients also accept a typed `host[:port]`.
- **Client identification:** requests to the JSON routes (`/v1/server`, `/v1/index`, `/v1/events`, pairing) carry `MMP-Client: <app>/<version> (<platform>)`, e.g. `matinee/1.3 (tvOS 26.1)`. Media routes (`content`, `art`, `lyrics` and signed URLs) don't require it, because players like VLCKit and AVPlayer open those URLs themselves and won't send it.
- **Too-old clients:** the server may answer `400` with `error.code = "client_too_old"` for a client version known to be broken. (Not `426`: RFC 9110 §15.5.22 requires an `Upgrade` header naming a protocol to switch to, which doesn't fit "update your app".)
- **Errors:** always `{"error": {"code": "<stable_snake_case>", "message": "<English, for logs>"}}` with a suitable HTTP status. Clients localize from `code`. v1 codes:
  - `not_paired`, `token_revoked`, `url_expired` (pairing, 1.1)
  - `pairing_expired`, `pairing_code_wrong`, `pairing_locked` (1.1)
  - `not_found`, `range_not_satisfiable`, `method_not_allowed`
  - `folder_unavailable`
  - `client_too_old`, `unsupported_feature`

## 4. Access: open by default, optional pairing code (1.1)

There is one household and no user accounts. Every client that can reach the server sees everything under its shared folders.

**Open (`auth: none`, the default, and the only mode in 1.0).** No token, code or password. Any device on the local network can read the index and the files under the shared folders, exactly as with the Sync server today. Clients skip the pairing screen, and media URLs are plain `/v1/items/{id}/content` URLs that VLCKit and AVPlayer can open directly.

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
   - **Items that are gone:** a signed URL for an item that's no longer in the index gets `404`, like every other route (§8).

**Switching modes.** Turning pairing on makes connected clients get `401 not_paired`; each shows the pairing screen once. Turning pairing off makes the server ignore tokens.

`GET /v1/server` never needs a token; it's what a client calls first:

```json
{
  "id": "6f1c…", "name": "Living Room Mac mini",
  "auth": "none",
  "protocol": {"major": 1, "minor": 0, "features": ["art-resize", "events"]},
  "software": {"name": "minimoon-media-server", "version": "5.0.0"},
  "sharedFolders": [{"id": "a41f…", "name": "Music", "available": true},
                    {"id": "0c9e…", "name": "Movies", "available": false}],
  "index": {"etag": "W/\"9f2c…\"", "filesEtag": "W/\"18d4…\""}
}
```

- **One tree.** All shared folders (`sharedFolders`) are merged into a single tree, each appearing as a top-level folder. Each has a permanent `id` and a display `name`, which is only a label (§5, §8).
- **Every client reads the whole index.** The server publishes everything under its shared folders, and every client reads all of it (§5). A client that copies files (MiniMoon on iOS) syncs the whole file list. There are no server-side or per-device subsets in v1.
- **`sharedFolders[].available`** is false when that shared folder is unavailable (§5). Only the files under that folder go offline.
- **`index`** holds the current ETags of the full index (`etag`) and of the file list (`filesEtag`) (§5), so a client polling `/v1/server` can tell whether anything changed without another request.
- **Pairing mode:** an unpaired client gets the same response with `sharedFolders` and `index` omitted.

## 5. Library sync: the full index, with an ETag

`GET /v1/index`, or `GET /v1/index?view=files` for the file list (below)

- **Response:** every item in every shared folder, and every folder needed to reach them, in one gzipped JSON response with an `ETag`. Hidden files and symlinks leading out of the shared folders (unless the user opted in) aren't listed (§9), nor are subfolders the user chose to publish without (below).

```json
{
  "folders": [ … ],
  "items":   [ … ],
  "offline": ["0c9e…"]
}
```

- **`folders`:** every ancestor folder of every item, up to and including the shared folder itself, so a client can build the tree without gaps.
- **`offline`:** the ids of shared folders that are currently unavailable. Their items are listed from the server's last good scan when it has one. After the index is rebuilt it has none, and the folder has no items in the response (see "No previous snapshot" below); the client rule for offline folders keeps the client's copy either way.
- **Unchanged index:** the client sends `If-None-Match: <etag>`; the server answers `304 Not Modified` with no body.
- **ETag:** changes whenever anything in the index changes: a file added, removed or changed, metadata re-read, a folder going offline or coming back, or a shared folder being added, removed or renamed. Clients treat it as opaque.
  - **Weak, shared by both encodings.** The index is served gzipped or plain depending on `Accept-Encoding`, and RFC 9110 §8.8.3.3 requires a strong ETag to differ between the two. Index ETags are therefore weak (`W/"…"`), the same for both encodings, and index responses carry `Vary: Accept-Encoding`. `If-None-Match` compares weakly, so `304` works for either, and `/v1/server` advertises that one ETag.
  - **A hash of the contents, never a counter.** The server derives it from a hash of the index's contents. A counter could restart after an index rebuild and repeat an old value for different contents.
- **Client update:**
  - **Apply all or nothing.** The client applies an index only after it has downloaded and parsed it completely, and applies it in one transaction. A truncated download or a parse error leaves the previous copy untouched.
  - **Compare by item `id`,** scoped to the server's `id`. Items in the local copy but not in the index were removed. Items new in the index were added. The client's own state key (below) is used only to look up its on-device state, never to decide which files exist, so two files that share a state key stay two files.
  - **Offline folders lose nothing.** For a shared folder listed in `offline`, the client adds and updates items from the response but never removes any. That holds whether the server sends its last snapshot or, after an index rebuild, no items for that folder at all. Removals under that folder are applied once it's back online.
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

  - **Each item** carries only the required `id`, `sharedFolderId`, `path`, `size`, `mtime` and `rev`, all as defined in the wire schema below. `size` is included because the iOS sync compares size as well as mtime (`SyncService.preparePlan`), and `rev` so that an in-place change the watcher caught with size and mtime unchanged still reaches the iOS app's copy. There are no folders, metadata or `kind`. A client builds folders from paths, and reads kinds and tags from the files it downloads.
  - **`offline`** is kept. A client must still not treat a missing shared folder's files as deleted.
  - **Its own ETag (`filesEtag`).** The file list is a different representation from the full index, so it has its own ETag, weak and hashed like the full index's. That ETag changes only when the list changes: a file added, removed, moved, or changed in size, mtime or `rev`, a folder going offline or coming back, or a shared folder being added or removed. Metadata re-reads don't change it, so a server-side reader upgrade doesn't make the iOS app download the list again. `If-None-Match` and `304` work as for the full index.
  - **The client rules are the same,** apart from metadata: apply the list all-or-nothing, compare by item `id`, never remove offline folders' items, and redownload media when `size`, `mtime` or `rev` changed. The iOS app stores the `rev` it downloaded alongside size and mtime. The metadata rule doesn't apply, because the client reads metadata from its own copies of the files.
  - **Size (estimated):** about 180 bytes per item before gzip (the example above), roughly a quarter of a full audio item, so about 7 MB for 40,000 tracks, much less gzipped. That's comparable to the Sync server's `/files` listing the iOS app downloads today.
- **No deltas, cursors or tombstones.** Removals, moves, folder deletes and files added then removed between syncs all fall out of the client's comparison. The server keeps no change history.
- **Scans never publish a guess.** A removal reaches clients only when the server has positively seen that a file is gone:
  - **Unavailable shared folders.** When a folder is added, the server records the identity of the volume it lives on (its device ID or volume UUID). Before every scan it checks that the folder exists, is readable, and is still on that volume. An unplugged drive or unmounted network volume usually leaves an empty mount point behind on the startup disk, so the volume check, not emptiness, is what detects it. If the check fails, the folder is marked `available: false`, its last snapshot keeps being served in the index (listed in `offline`), and its files' content, art and lyrics answer `503 folder_unavailable`.
  - **A scan error publishes nothing.** A scan error means failing to list a folder or to read a file's size and modification date: a permission error, an I/O error, a volume dropping partway through. Failing to read a file's metadata is not a scan error; that file is still listed with its required fields (§8, reading metadata safely). If a scan error happens under a shared folder, the server keeps that folder's previous snapshot whole and retries on the next scan. After repeated failures the desktop app shows which subfolders can't be read and offers to publish without them. That's an explicit choice by the user, never a silent partial publish. (Keeping only the failed subfolders' previous contents is a possible refinement later; v1 keeps the whole folder.)
  - **A scan that completes is the truth,** including a folder that's really empty: if it completes without errors, its removals are published.
  - **One step.** A completed scan replaces the folder's snapshot in a single database transaction, and the index ETags are computed after it. A client never sees half a scan.
  - **No previous snapshot.** On the first scan of a newly added folder, or after the server's index has been rebuilt from scratch, the folder is published only once a scan completes without errors. Until then it's listed as unavailable, not partial, and listed in `offline` with no items. That matters after a rebuild, when clients still hold copies the server no longer knows about: the client rule for offline folders (above) keeps them. A rebuild loses only the index; shared folders, their UUIDs and volume identities live in the server's settings (§8), so they survive it.
- **Shared folder removed in the desktop app:** it drops out of `/v1/server` and its items drop out of the index, so clients remove them through the normal comparison. Only a shared folder that is still listed but unavailable keeps its items on clients (`offline`, above).
- **Size (estimated, not measured):** the example audio item below is about 650 bytes of JSON, so 40,000 tracks is roughly 25 MB, a few MB gzipped. A video item is somewhat larger because of its streams, but video libraries hold far fewer files. Parsing and comparing a 100,000-track library on an Apple TV should be measured before relying on it. If it proves too slow, a delta endpoint can be added later as an optional feature, since v1 changes are additive.

**Wire schema**

- **Conventions:**
  - **Absent means unknown.** A field the server couldn't determine is left out, never sent as `null` or guessed. Fields marked required below are always present.
  - **Units:** `size` in bytes; `mtime` in milliseconds since the Unix epoch (UTC); durations in milliseconds; sample rates in Hz; bitrates in kbit/s.
  - **Paths:** a `path` is relative to its shared folder and written with `/` between components. File names come from the file system as UTF-8, without normalization, and clients compare them exactly. A shared folder's own `path` is `""`. File names that aren't valid UTF-8 are skipped and logged. (`/` can't occur inside a file name on macOS, Windows or Linux, so the separator is unambiguous.)
- **Folder:** required `id`, `sharedFolderId` (the shared folder it belongs to), `name`, `path`; `parentId` on every folder except a shared folder itself.

  `{"id": "f3", "sharedFolderId": "a41f…", "parentId": "f1", "name": "Kind of Blue (1959)", "path": "Miles Davis/Kind of Blue (1959)"}`

- **Item:** required `id`, `sharedFolderId`, `parentId` (the folder containing it), `name`, `path`, `kind`, `size`, `mtime`, `rev`. `mime`, `audio`, `video` and `tags` appear when known.
- **`rev`** (non-negative integer, 0 for most files) is a content revision for changes that size and mtime don't show. Some taggers rewrite a file in place, keep its size and put the old mtime back.
  - **Detected from file-system events, not from metadata.** The server's file watcher (§8, change detection) reports a write to a file even when its mtime is set back afterwards: FSEvents `ItemModified` on macOS, `IN_MODIFY`/`IN_CLOSE_WRITE` with inotify on Linux, a last-write change from `ReadDirectoryChangesW` on Windows. When a write event arrives for a file whose size and mtime then match the index, and whose ctime has moved since the index recorded it, the server bumps its `rev`. It never infers a change by comparing metadata, which can't tell changed bytes from a changed reader or a failed read.
  - **Why the ctime check.** FSEvents can repeat a file's `ItemModified` flag in later events for the same file, and a write's event can arrive after the scan that already saw its new mtime. Either would bump `rev` for content clients already have. The ctime advances on every write and attribute change and no program can set it back, so a write event counts only if the ctime moved. The index keeps the ctime from the last time the file's content was read, not refreshed on attribute-only changes, so a replayed write still finds it moved. The cost is an occasional extra bump when a repeated write flag comes with a later attribute change, which costs one download. Windows has no ctime in the standard library, so there the event alone decides.
  - **A write a scan caught midway.** If a scan lands while a long write is under way, it records the temporary mtime; when the tagger then puts the old mtime back, the file's size and mtime can return to values clients already hold for the old bytes. So a file whose content changed but whose mtime didn't move forward also gets a new `rev`.
  - **Never reused.** A bump takes the next value of a server-wide counter, and both the counter and each file's bumped `rev` are kept in the server's settings, not in the index (§8). An index rebuild therefore gives every file the same `rev` as before, so changed content can't look like a client's old copy. Bumps are rare, so this list stays small; entries for files that no longer exist are pruned.
  - **What it drives:** it's part of the content ETag (§6), so downloads, `If-Range` and the art cache all see the change, and clients redownload media when it changes.
  - **Limits (best effort):** events the watcher never sees are missed:
    - **while the server isn't running:** on macOS the watcher resumes from its last FSEvents event id, kept in settings, so writes made meanwhile are replayed unless the system has purged that history; on Linux and Windows they're missed;
    - **when events overflow** (FSEvents `MustScanSubDirs`, inotify `IN_Q_OVERFLOW`, a `ReadDirectoryChangesW` buffer overflow): the server rescans that subtree by size and mtime, which can't see an mtime-preserving write, and logs it, rather than bumping every file in it;
    - **on network volumes** the host mounts, whose events are unreliable (§11).

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
  - **`rating`** is the number the file holds, on that format's own scale: 0–100 for an MP4 `rate` atom, 0–255 for a Vorbis comment `RATING` (as MiniMoon writes it), and 0–255 for an ID3 `POPM` frame, rounded up to the next multiple of 51 so every tagger's byte for a star count reads as that many stars (Windows Media Player and MusicBee write 1, 64, 128, 196 and 255; Picard writes multiples of 51). A `POPM` byte of 0 means unrated and gives no `rating`. Clients know the format from `mime` and keep their own ratings locally.
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
  - **Unreadable files:** a file whose metadata can't be read is still listed and served, with only the required fields and `mime` (which comes from the extension). Matinee falls back to probing it with VLCKit as today.
- **Identity:**
  - **`id`** is a hash of the shared folder's `id` and the item's `path`. It survives server index rebuilds and changes only when the file is moved or renamed on disk. Renaming a shared folder's display name changes nothing. Used in content, art and lyrics URLs.
  - **Clients key their on-device state by facts they already use, scoped to the server's `id`,** so two servers with the same paths never collide:
    - **MiniMoon:** (server `id`, shared folder `id`, `path`), its path identity today with the folder and server added. A file moved on disk loses its state, as today.
    - **Matinee:** (server `id`, `name` + `size`), the key it already gives share files (`ShareKeys.ownKey`), so its existing share machinery applies. A file moved to another folder keeps its state; a renamed one doesn't. Two different files with the same name and size share state; that's Matinee's existing trade-off for share files, accepted rather than fixed. They're still two catalog entries, because which files exist is decided by item `id` (§5, client update).
  - **No content fingerprint in v1.** A hash over the start of a file changes whenever a music file is retagged, because ID3 and FLAC tags sit at the front. Neither app needs one for server files. It can return as an optional feature if deduplication ever needs it.
- **Push:** with the `events` feature, `GET /v1/events` is a Server-Sent Events stream sending `index-changed {etag, filesEtag}`, `folder-availability {sharedFolderId, available}`, and `shared-folders-changed` when a shared folder is added, removed or renamed. A client that falls behind the stream gets `shared-folders-changed`. Events are hints only. Clients act on them by re-checking `/v1/server` and `/v1/index` with `If-None-Match`, and do the same after every reconnect, because events sent while disconnected are lost. Clients that don't use events re-check when they come to the foreground.

## 6. Content, artwork, lyrics

- **File bytes:** `GET /v1/items/{id}/content`, or in pairing mode the signed `/v1/c/{id}?…` (§4).
- **HTTP contract for content:**
  - **Headers:** `Accept-Ranges: bytes`, `Content-Length`, the real `Content-Type`, `Last-Modified`, and a strong `ETag`.
  - **No content coding.** Media is always sent as the file's own bytes, never with `Content-Encoding`, so each file has one representation and one ETag.
  - **ETag:** built from exactly the facts clients compare in the index, `size`, `mtime` in milliseconds and `rev` (`"<size>-<mtime ms>-<rev>"`, §5). Using the index's precision means the ETag and a client's download decision can't disagree about whether a file changed. It's declared strong (RFC 9110 §8.8.1) on the assumption that any change to a file's bytes changes its size, its mtime to the millisecond, or its `rev`. That assumption breaks when the watcher misses an mtime-preserving write (§5, `rev` limits), or when two same-size writes land within one millisecond. A strong ETag is needed because RFC 9110 §13.1.5 forbids clients from sending a weak one in `If-Range`.
  - **Ranges:** a single range per request, in any of the three forms (`bytes=a-b`, `bytes=a-`, `bytes=-n`), answered with `206` and `Content-Range`. A request for several ranges gets the whole file with `200`, which RFC 9110 allows (§14.2). A range starting past the end gets `416` with `Content-Range: bytes */<size>`.
  - **Conditional requests:** `If-Range` with the ETag (a stale ETag gets the whole file with `200`), and `If-None-Match` (`304`). A date in `If-Range` gets the whole file, and `If-Modified-Since` is ignored, because neither can see a change that kept the mtime.
  - **HEAD** returns the same headers as GET, with no body.
  - **Files that change mid-transfer (best effort).** With `Content-Length`, a response is complete as soon as its last byte arrives (RFC 9112 §6.3), so closing the connection afterwards can't make the client reject it. The check therefore comes before the last byte:
    - the server records the file's size and mtime (at full precision) when a response starts;
    - after reading the response's final chunk from disk, and before sending it, it checks them again, and checks whether the watcher has reported a write to that file since the response started;
    - if anything changed, it closes the connection without sending that chunk, so the client sees an incomplete transfer, not a silently mixed file.
    This catches a write that updates the mtime, and a write the watcher reports even if the mtime is put back afterwards. It misses a write whose mtime was restored and whose event hasn't arrived or was lost, and on network volumes the host mounts, attribute caching and unreliable events can hide a change. The whole mechanism is best effort. Apps that save by writing a new file and renaming it over the old one are safe anyway on macOS and Linux, because the server keeps reading the file it opened.
  - **Across range requests.** A player seeking through a file makes many separate requests. Each response carries the ETag, but a changed file is caught across requests only if the player sends `If-Range`, or compares the ETag itself. Whether VLCKit and AVPlayer do is unverified and is part of the playback test (§10). Until that's known, consistency across a playback session is best effort.
  - **Interrupted downloads** resume with `Range` plus `If-Range`:
    - **`206`:** the client checks that `Content-Range` starts exactly at the length of its partial file, appends the body, and checks the finished file's length against the total in `Content-Range` (the `/<size>` part). `Content-Length` on a `206` is the length of the range, not of the file.
    - **`200`:** the file changed in the meantime, or the server ignored the range. The client discards its partial copy and writes the body from the start, checking it against `Content-Length`.
  - **MiniMoon:** downloads to its cache, checks the finished file's length as above, then opens it with `AVAudioFile`.
  - **Matinee:** hands VLCKit the URL and streams directly. Whether VLCKit's HTTP access plays, seeks and recovers correctly against this server is the first thing to test once a prototype server exists (§10).
- **Embedded artwork:** `GET /v1/items/{id}/art` returns the first embedded picture, or `404`. For video, that's a Matroska cover attachment or the MP4 `covr` atom. For an `image` item it's the image itself. Same headers and conditional-request rules as content.
- **Resized images** (`art-resize` feature): `?maxSize=600` scales either an item's embedded art or an `image` item's content to fit within 600 pixels, as JPEG. Results are cached in the server's app-data directory, never next to the media. The cache key is the item `id`, its content ETag, the requested `maxSize` and the resizer's version number, so a changed source, a different size or an improved resizer each produce a new entry.
- **Embedded lyrics:** `GET /v1/items/{id}/lyrics` returns `text/plain`.

## 7. Client-side state (not part of the protocol)

The server stores nothing on behalf of clients. Each app keeps its state where it does today:
- **Matinee:** `PersistentRecord` in UserDefaults on the Apple TV (400 KB budget). A share file gets a record entry only once it has state worth keeping (watched, a position, a Fix Match or rename, a track choice); its automatic match lives in a purgeable cache sidecar. MMP files would follow the same rule.
- **MiniMoon:** its own library database on each device.
- **Syncing state between devices** is a separate decision outside MMP. Matinee's candidate is iCloud (`docs/monetization.md`).

## 8. Server behaviour

- **Shared folders:**
  - **Shared folders** are the folders on disk the user shares, added in the desktop app or with CLI flags. The Sync app has one today; MMP allows several. They're merged into one tree, each as a top-level folder.
  - **Identity versus label.** Each shared folder gets a permanent UUID when it's added. Identity (§5) and availability use that UUID. Its display name defaults to the folder's name on disk; a clash gets a number added (`Music 2`). The user can rename it freely, since renaming changes only the label.
  - **No selections.** The Sync app's file-selection tree and presets aren't carried over. The server publishes everything under its shared folders, and every client reads all of it (§4).
- **Enforcement:** an item is served (`/content`, `/art`, `/lyrics`, signed URLs) only if it's in the index: inside a shared folder and not filtered out (§9). Anything else, including an item that has since been removed, gets `404`, as if it didn't exist. Enforced by the server on every route, not only in listings.
- **Index:**
  - **Contents:** a SQLite database in the app's data directory, holding files, folders, their metadata, and each shared folder's last snapshot.
  - **Reader version and read status:** each indexed file records the version of the metadata reader that read it, and how the read went:
    - **read:** metadata extracted;
    - **failed temporarily:** a timeout or I/O error, which may succeed later;
    - **unreadable:** an unsupported or malformed file, which won't.
  - **Settings, not index:** everything that can't be rebuilt from the disks lives in the server's settings, so rebuilding the index from scratch never changes an identity:
    - the server's `id`;
    - the shared folders: UUID, path on disk, display name and volume identity;
    - each file's bumped `rev` and the counter they're drawn from (§5), and the watcher's last FSEvents event id on macOS;
    - when pairing is on, paired devices and the URL-signing key.
  - **Why it exists:** so metadata isn't re-read on every request; the Sync server's `/files` walks the whole tree on every call.
  - **Ready responses:** the server keeps the index and the file list ready, each gzipped on first use, and rebuilds them when the index changes.
- **Change detection:**
  - **Triggers:** file-system watching, plus a full rescan at startup and periodically. The watcher subscribes to file-level events (on macOS, an FSEvents stream with `FileEvents`), because detecting in-place writes (§5, `rev`) needs to know which file was written, not only which folder changed.
  - **Re-reading metadata:**
    - files whose size or mtime changed are re-read;
    - files whose `rev` was bumped are re-read;
    - files read by an older reader version are re-read in the background after a server update, so reader fixes reach existing files and then clients, through the metadata rule in §5, without any media being downloaded again;
    - files whose read failed temporarily are retried on later scans, waiting longer after each failure, up to a fixed number of attempts; after that they're treated as unreadable. Recovering from a failed read is never taken as a sign that the file changed;
    - unreadable files are re-read only when their size, mtime or `rev` changes, after a reader upgrade, or on "Reindex everything";
    - a "Reindex everything" command in the desktop app re-reads every file's metadata. It never changes `rev` or makes clients download media again; content changes come only from size, mtime and the watcher.
  - **Scan safety:** the volume check, scans that publish nothing after a scan error, and one-step publication are as in §5.
- **Tag reading: a copy, not a dependency.**
  - **What's copied.** The server doesn't depend on the apps' `minimoon_tags` crate. It carries a copy of only the reading side: `read_tag` and its helpers, the native-text and sort-tag reading, and embedded-picture reading. The crate's writing and tag-copying code, and its C interface for Swift, stay out.
  - **Why.** The crate is a thin layer over `lofty`, so the copy is small. The server avoids building the apps' static library and its Swift-facing interface, and it doesn't carry write code it must never call (§2, read-only).
  - **`lofty` version.** The crate pins `lofty` to a git revision for an ID3v2 write fix. The read-only copy uses the crates.io release 0.25.5. It read 1,011 sample files identically to the pinned revision (§11).
  - **Keeping them aligned.** Advantage 3 (§1) depends on the server and the apps reading tags the same way, and a copy can drift. A comparison test runs both readers over the same sample files and fails on any difference in the fields MMP sends. When the apps' reader changes, the copy is updated and its reader version (§8, index) is bumped, so existing files are re-read.
- **Reading metadata safely:**
  - a failure to read a file's metadata never stops the file being listed or served (§5);
  - the reader has a time and size limit per file, and a file that exceeds it is indexed with the required fields only;
  - errors are logged and shown as a count in the desktop app, split into temporary failures and unreadable files.
- **Sleep:**
  - **While streaming:** while any content response is streaming, the server holds a "prevent idle sleep" assertion on macOS (and the Windows equivalent).
  - **Grace period:** the assertion is kept for a grace period after the last request, because players buffer ahead and can go quiet for a while in the middle of playback. Its length needs tuning against real buffered playback in VLCKit and AVPlayer.
  - **Optional setting:** "Keep this computer awake while sharing".
  - **Clients** show a distinct "server asleep or off" state and keep browsing from their local copy.
- **Network visibility:** the desktop app shows "Sharing on ‹network›" and has a one-click pause, because in open mode a laptop shares its folders with any network it joins (§9).

## 9. Security in v1, and what's deferred

- **Open in 1.0.** Anyone on the same network can read everything under the shared folders. That's the same exposure as today's Sync server, which has no auth.
  - **The real risk is a laptop that moves:** with the server running, a MacBook on hotel or café Wi-Fi advertises itself and serves its shared folders to everyone there.
  - **1.0 mitigation:** the network name, and the pause control (§8).
  - **1.1:** the optional pairing code (§4).
  - **Possible later:** remember the network chosen at setup and pause sharing on any other. Reading the Wi-Fi name on recent macOS may need Location permission (from memory, not checked).
- **No per-device restrictions.** Every client can read everything under the shared folders. Files outside them are never served.
- **Nothing to damage.** Because no route writes, an unwanted client can read the published files but can't change, delete or add anything, or alter another device's state.
- **Pairing mode (1.1):** a code shown on the server proves consent; every API call and media URL is authenticated, and revoking a device makes its next request fail and closes its open downloads (§4).
- **Plain HTTP.** Media, and in pairing mode the token, can be sniffed by anyone on the same LAN.
  - **Why plain:** TLS with a self-signed, pinned certificate works in URLSession through its challenge delegate, but it's unverified for VLCKit's HTTP access and for AVPlayer without a resource loader.
  - **Later:** a `tls` feature with the certificate fingerprint pinned at pairing, once VLCKit's behaviour is tested.
- **Inside shared folders only.** The server never exposes paths outside the shared folders, filters hidden files, rejects path traversal, and never follows symlinks out of the shared folders unless the user opts in. This matters more in open mode. The Sync server's `/file` route skips some of these filters, so it's not a model to copy.

## 10. Smallest first version

**Where the server stands (2026-11-15).** Steps 2–5, 7 and 8 below are built, and so is the server side of protocol 1.1 (pairing, signed URLs, `events`, `art-resize`):
- **Not done:** step 1, the playback test, needs Matinee and MiniMoon builds that speak MMP. Step 6 belongs to the desktop app (`minimoon-sync`), which still embeds the old server: the server exposes what that UI needs (shared folders, excluded subfolders, pausing, pairing and devices, error counts, "Reindex everything") but nothing calls it yet.
- **Tested:** on macOS only. The Windows code paths were type-checked, and the Linux ones through an Android target that shares them; neither was built or run on its own platform.
- **Additions beyond the steps:** the ctime check on `rev` bumps (§5), and an idle-sleep assertion while content streams (§8).

**Core (protocol 1.0)**

- **Server:**
  1. **A playback test first.** Build a minimal `/content` endpoint with the HTTP contract in §6. Before anything else depends on it, check that VLCKit (Matinee) and AVPlayer/URLSession (MiniMoon) play, seek, and recover from a dropped connection and a changed file, including whether each sends `If-Range` or checks the ETag across range requests.
  2. Rename and rebrand the server, and remove the Sync protocol's routes. An upgraded server no longer answers older MiniMoon iOS versions, so an app version that speaks MMP has to ship before or with it; the apps own that transition. Keep the bundle identifier (`com.plastaq.sync-server`) and the updater endpoint so existing installs upgrade in place. Whether the Tauri updater handles a product-name change cleanly is unverified.
  3. `/v1/server` and Bonjour advertising, open mode only.
  4. **The index:**
     - shared folders (UUIDs, paths, names, volume identities), bumped `rev` values and their counter in the server's settings, and a SQLite index of files and metadata, with reader versions and read status;
     - a file-level watcher that bumps `rev` on in-place writes (§5), resuming from its last event id on macOS. The `notify` crate (8.2.2) covers Linux (inotify, with a watch per directory and `IN_Q_OVERFLOW` reported as a rescan) and Windows. Its FSEvents backend always starts from "now" and doesn't expose event ids, so replay after a restart on macOS needs the server's own FSEvents stream, or a patch to `notify` that exposes the starting id;
     - the scan-safety rules in §5: the volume check, nothing published after a scan error, one-step publication, no partial first publish;
     - `/v1/index` with weak ETags and `304`, in both views (`full` and `files`), each with its own ETag.
     This is the largest server piece.
  5. `/content` (from step 1), `/art` and `/lyrics`.
  6. **Desktop app UI:**
     - shared folders and their display names;
     - the sharing network and a pause control;
     - read-error and metadata-error reporting, and "Reindex everything".
     The Sync app's selection tree and presets go away; every device gets everything under the shared folders (§4).
  7. **Tag reading, copied in.** Copy the reading side of `minimoon_tags` into the server (§8), and extend the copy with duration in milliseconds (the crate reports whole seconds, `properties.duration().as_secs()`) and a channel count. The apps' crate doesn't change.
  8. **Video reading in native Rust**, chosen by the reader test ([video-reader-test.md](video-reader-test.md), 2026-11-14):
     - **Crates:** `matroska-demuxer`, `mp4-atom`, `scuffle-h265`, `h264-reader`, `scuffle-av1`, and `lofty` for MP4 tags.
     - **Hand-written parsing:** about 240 lines, covering the Dolby Vision configuration record, Matroska attachment headers, raw EBML and MP4 box scanning (`mp4-atom` drops `dvcC`/`dvvC`), and the HDR10+ SEI check.
     - **Results:** on 107 real files, it agreed with ffprobe and MediaInfo on duration, codec, size, transfer, Dolby Vision, HDR10+ and covers for every file. The tools differed on frame rate, channel count, language and title for some files, as listed in the report. On 10 synthetic HDR clips it detected every HDR10, HLG, HDR10+ and Dolby Vision 5 and 8.1 case, but didn't read the MP4 forced flag. It ran in a median of 19 ms and read a median of 100 KB per file. ffprobe read 5 MB per file and would need a custom LGPL build; MediaInfoLib is an 8 MB C++ library needing FFI. The test is not comprehensive format validation; its gaps are listed in the report and in §11.
     - **Done since the test:** MP4 forced flags (ffmpeg's `kind` box), default flags and track titles (`udta/name`); HE-AAC with SBR reported as 2 channels unless its config rules parametric stereo out; `trackNumber` on every stream. On the same 117 files nothing regressed, the three HE-AAC trailers now agree with ffprobe, and the synthetic MP4s' forced flags are 10/10.
     - **Placement:** in the server, next to the copied tag reader.
- **Shared Swift client package:** used by both MiniMoon and Matinee, as a new Swift package with its own repo, since Matinee and MiniMoon are separate repos. It covers:
  - discovery (`NWBrowser`);
  - fetching `/v1/index` in either view with `If-None-Match`, and applying it all-or-nothing in one transaction, off the main actor;
  - comparing by item `id`, with app state keys kept for state lookups only;
  - the metadata rule;
  - offline folders, never removing their items;
  - content URLs with resumable downloads checked against `Content-Range`, and an error mapping.
- **MiniMoon:**
  - **iOS (copies files):** syncs from the file list (`?view=files`). It syncs the whole list, downloading added and changed files and rescans them locally, as it does today against `/files`. This replaces its Sync-protocol client for MMP servers, with offline folders and resumable downloads added.
  - **macOS and tvOS (no local copy):** an MMP `LibrarySource` mapping items from the full index to `Track` rows, plus fetch-to-cache playback.
- **Matinee:** an MMP source feeding `LibraryStore` the way a share does, filling `ProbeResult` from each item's `video` facts instead of probing, and matching streams to VLCKit tracks by `trackNumber`. It plays content URLs through VLCKit. Its existing share machinery (record keys, the state-worth-keeping rule, offline state, the title cap) applies.

**Next (protocol 1.1, already designed above; the server side is built)**

- Optional pairing code, tokens and signed URLs bound to device and generation (server setting, client pairing screen, Keychain storage).
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
  - The video reader test: native Rust, ffprobe and MediaInfo agree on duration, codec, size, transfer, Dolby Vision, HDR10+ and covers for all 107 real files. On 10 synthetic HDR clips all three detected every HDR format and read the audio tracks, tags and covers correctly; the Rust probe didn't read the MP4 forced flag ([video-reader-test.md](video-reader-test.md)).
  - Matinee probes each video once and caches a `ProbeResult` in a purgeable sidecar (`Playback.swift`, `LibraryStore.swift`). It holds duration, size, video codec, dynamic range, frame rate, the first audio track's codec and channels, and audio and subtitle track counts. Local MP4 and MOV files go through AVFoundation; everything else, including share files, goes through a VLCKit parse with a 10-second limit (`VLCKitProber` in `VLCKitEngine.swift`).
  - The Sync app's selection: a tree of folders and files saved as named presets in web storage, one active at a time (`minimoon-sync/src/state.ts`). Folder node ids end in `/`. The server receives the active preset as `selected_roots` and filters `/files` by path prefix, with an empty list meaning everything (`list_files_handler`).
  - MiniMoon's iOS sync already downloads the full `/files` list and compares by library-relative path, size and mtime (`SyncService.swift`); the comparison once blocked the main actor on a large library (MINIMOON-B9).
  - The Sync server has no auth.
  - `lofty` reads no video fields and fails on Matroska; it reads MP4 tags and covers correctly (video reader test).
  - The tag reader's fields (`tagging/src/tag.rs`):
    - it has no movement field;
    - it reads both a year field and a recording date, whose year it keeps;
    - `rating` is a raw number: an MP4 `rate` atom holds 0–100, and MiniMoon writes Vorbis `RATING` as 0–255 (`native_text.rs`).
- **Standards:**
  - RFC 9110 §15.5.22: a `426` response must carry an `Upgrade` header.
  - RFC 9110 §13.1.5: clients must not send a weak ETag in `If-Range`.
  - RFC 9110 §8.8.3.3: a strong ETag for a gzip-encoded representation has to differ from the unencoded one's.
  - RFC 9110 §14.2: a server may ignore a request for several ranges.
  - RFC 9112 §6.3: with `Content-Length`, a message is incomplete only if the connection closes before that many bytes arrive, so closing afterwards doesn't make a complete response fail.
- **Crates:**
  - `notify` 8.2.2 (from its source): watches inotify `CLOSE_WRITE` and `ATTRIB`, reports `IN_Q_OVERFLOW` and dropped FSEvents as `Rescan`, adds a watch per subdirectory for recursion, and subscribes to file-level FSEvents. Its FSEvents stream starts at `kFSEventStreamEventIdSinceNow`, with no way to pass a starting event id.
- **The server build (tests in `tests/`, run on macOS 27 on APFS):**
  - An in-place write that restores the file's mtime is reported by FSEvents as `ItemModified` and bumps `rev`. A write made while the server is stopped is replayed from the stored event id and bumps `rev` after the restart. FSEvents repeated a file's write flag in a later event, which bumped `rev` twice until the ctime check was added.
  - A transfer of a file written mid-transfer (mtime restored) is cut short before its last chunk. Revoking a device closes its open download and its event stream; pausing closes event streams too.
  - The `rev` counter, ids and `rev` values survive a restart and an index rebuild. A folder on an unmounted volume keeps serving its last snapshot and is listed in `offline`. A scan error publishes nothing until the user excludes the unreadable subfolder.
  - The copied tag reader on crates.io `lofty` 0.25.5 read 1,011 sample files (MP3, WAV, M4A, AIFF, FLAC, MPC, APE, Ogg, WavPack, Opus, Speex) identically to the apps' crate on its pinned revision.
  - ID3 `POPM` ratings and unsynced lyrics (ID3 `USLT`, Vorbis `UNSYNCEDLYRICS`) were gaps in the apps' reader, which the copy matched: `lofty` presents `POPM` as `email|stars|counter` text, and maps unsynced lyrics to `ItemKey::UnsyncLyrics`, which the reader didn't look at. The apps' reader now reads both, and the copy (reader version 2) has the same change, checked by the apps' tests for it on MP3, WAV and FLAC files. The 1,011-file comparison above predates that change.
  - A first scan of 29,722 local files (107 videos and Logic Pro's sample library) took about 5 seconds. Its index is 13.4 MB, 1.1 MB gzipped.
- **Apple and competitors:**
  - **Local network privacy:** tvOS doesn't have it (TN3179).
  - **Plex:** remote playback of personal video needs Plex Pass or a Remote Watch Pass since April 2025, enforced in stages. Local playback is unaffected.
  - **Jellyfin:** its setup wizard creates an admin account. The server is GPL-2.0.
  - **Swiftfin:** the Jellyfin tvOS client is video-only, per its 2022 announcement and a 2026 third-party guide. Its current release isn't checked.
  - **Subsonic:** clients use an empty `search3` query for a full sync.
  - **Home Sharing:** the Computers app is still documented for tvOS 26.

**Unverified**

- Whether Jellyfin or OpenSubsonic have a change feed.
- What users actually use Sync app presets for. Presets aren't migrated, so anyone who used them to limit what the iOS app copied now gets everything under the shared folders.
- Index size and the time to parse and compare it on an Apple TV for a 100,000-track library (only estimated).
- VLCKit and AVPlayer playback, seeking and recovery against this server's HTTP contract (the first step in §10), including whether either sends `If-Range` or checks the ETag across range requests.
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
- The inotify watch limit (`fs.inotify.max_user_watches`) on the Linux distributions users run, for libraries with many directories. From memory, kernels since 5.11 size its default from RAM; older ones default to 8,192.
- That file-level watching reports a write when a tagger restores the old mtime on Linux and Windows (tested only on macOS, below), and how long FSEvents keeps history for replay. Both should be tested with the taggers users actually run.
