# Loopback Media Server

A small axum HTTP server bound to `127.0.0.1:<os-assigned-port>`
(`pollis-core/src/media_server.rs`), spawned at startup and shut down via
`AppState::shutdown()`'s graceful-drain signal. It is how large/decoded media
bytes reach the WebView **without** riding Tauri's IPC `Channel` — per-frame JS
dispatch stalls on V8 GC, so bulk media is served over loopback instead. Two
token-gated routes (same secret, 403 on anything else):

- **`GET /{token}/{hash}`** (`serve_media`) — the decrypted bytes of a cached
  media file (`{hash}` is the attachment's random file id since #1382; for public
  objects and emoji it is still a content digest), honouring single-range `Range` requests. The
  decrypted plaintext is handed out as a `Bytes` (`Bytes::from(Vec)`), so a range
  slice is a zero-copy view into it.
- **`GET /ws/screenshare/{token}`** (`ws_screenshare`) — a WebSocket that streams
  **decoded video frames** — remote screen shares and cameras, and the sharer's
  own previews — as binary messages (packed I420, the `pack_frame_bytes` wire
  format). The renderer uploads the Y/U/V planes into a WebGL
  YUV→RGB shader — the transport the `rustwebrtc` PoC proved sustains 1080p60+
  where per-frame Tauri IPC `Channel` dispatch stalled on V8 GC (#305 Phase 1).
  One frame stream serves every track; the renderer dispatches by the `track_key`
  in each frame header. (The per-frame IPC `Channel` it replaced,
  `subscribe_screen_share_frames`, is gone.)

Both routes gate on the one `token_ok` helper — a constant-time compare against
the secret minted at unlock, where a missing token counts the same as a wrong one
— so a route added later cannot pick up a subtly different rule.

Three subsystems write into the one content-addressed cache and are served by
the same `GET /{token}/{hash}` route without it knowing which: message
attachments (`commands::r2::get_media_url`), custom emoji
(`commands::emoji::get_emoji_url`), and — since #874 — public profile objects,
i.e. avatars and group icons (`commands::r2::get_public_file_url`). The first two
build their URL with `media_server::loopback_url`, which errors when the port or
token is missing. `get_public_file_url` keeps its own copy on purpose: it returns
an EMPTY string in that case, because the frontend treats empty as "fall back to
the byte path" and an error as a failure. Do not fold it in. The route
resolves a hash by scanning for `<hash>.<ext>.enc`, so a new producer needs no
server change at all; what it needs is a content-addressed name, which is why
avatars had to stop living at a mutable `avatars/{user_id}` key first.

Mobile has no loopback server: `commands::avatar::get_avatar_path` writes small
square renditions of the hash-checked avatar to the app's cache directory
instead (see `commands.md` → "Profile pictures"). Like custom emoji there, they
are public objects and stay cached across launches; `endSession()` wipes them.
Desktop's copy stays encrypted at rest; mobile's relies on the OS sandbox and
file protection, the same as its emoji cache.

## Saving an attachment out of the app

The loopback route is for RENDERING. "Save attachment as…" does not use it:
`commands::r2::save_media_to_path` downloads, decrypts, verifies the plaintext
SHA-256 carried in the attachment reference (`sha256`), and writes the file itself, so the bytes never round-trip through the
webview.

That is not only a bandwidth point. An attachment is a file a stranger sent, and
when the renderer fetched the loopback URL and wrote it with the fs plugin, the
file landed on disk with **no provenance marker** — no `com.apple.quarantine` on
macOS, no `Zone.Identifier` stream on Windows. Gatekeeper, SmartScreen, Office's
Protected View and the script-host warnings all key on exactly that marker, so
every one of them stood down for anything saved out of Pollis. `pollis-core/src/downloads.rs`
writes the bytes and applies the marker in **one** function
(`write_downloaded_file`) precisely so a future save path cannot do the first
without the second, and the export paths (`commands::export`,
`commands::export_fetch`) use it for the attachments they write out. There is
deliberately **no** `LSFileQuarantineEnabled` backstop in `src-tauri/Info.plist`:
that key stamps every file the process writes, including the bundle the in-app
updater installs, and a bundle stamped by Pollis itself is translocated on its
next Finder launch and can never update again. Releases that shipped the key
left such bundles behind; `commands::install_kind` removes Pollis's own stamp
(never a browser's) from the standard install locations at startup and before an
update, and reports `translocated_healed` so the update screen asks for a
relaunch rather than the Finder move. Linux has no enforced equivalent and the
function says so (`Marking::Unsupported`) rather than reporting a success it did
not achieve.

The command's target path goes through the same `PathScope` gate as every other
path-taking command (see `.codesight/wiki/commands.md`).

This fits the "media is Rust-first" architecture (see [overview.md](./overview.md)):
the renderer's WebRTC is intentionally unused; IPC carries UI events only, never
media bytes.

## Images a webview can't decode

iPhones store photos as HEIC, which WebKitGTK (Linux) and WebView2 (Windows) cannot draw. Two halves:

- **Sender:** every mobile photo-library pick passes `COMPATIBLE_PICK` (`mobile/lib/pickerOptions.ts`), so iOS hands over JPEG (and H.264 for HEVC video) instead of copying the original. `mobile/tests/picker-compatible.test.ts` fails on a picker without it.
- **Receiver:** when an image whose type only some webviews decode (`MAY_NOT_DECODE` in `AttachmentDisplay`: HEIC/HEIF, TIFF, AVIF) fails to render after its one retry, it shows as the ordinary file card, with name, size and Download, never an `err` tile, so photos already sent as HEIC can still be saved and opened in the OS viewer. macOS decodes HEIC, so it still shows inline there. Other types keep the blurhash / `err` path, since their failure means missing bytes, not an unsupported format. `e2e/undecodable-image.spec.ts`.

## Every media response is `no-store` (#1000)

The plaintext this server hands out must not end up in the WebView's own
on-disk HTTP cache — a cache the app does not own, does not know the location
of, and never clears (the media-cache wipe on logout reaches
`media-cache/<user>/`, not WebKitGTK's or WebView2's store).

A 200 carrying `Content-Type` and `Content-Length` and **no** cache directives
is storable by default (RFC 9111 §3), which is exactly what `serve_media` used
to return, so every image, audio clip and video the user opened was eligible to
be written to disk in the clear by the engine. Every response now carries
`Cache-Control: no-store, no-cache, must-revalidate, max-age=0` plus
`Pragma: no-cache`.

Structurally, not by discipline: `serve_media` builds no response bodies
itself. All three exits (200, 206, 416) hand a half-built `Builder` to
`media_response`, which is the single place the cache directives and the CORS
headers are attached. The **206 matters as much as the 200** — video seeking is
served entirely from the range path, so directives attached only to the full
body would leave every scrubbed video in the engine's cache.
`decrypted_bytes_are_never_storable_by_the_webview` asserts the headers on real
HTTP responses from a real spawned server, and
`serve_media_never_builds_a_response_body_itself` keeps the funnel from growing
a second exit.

## The per-user cache directory is a validated id, never a path

Layout: `<app_data_dir>/media-cache/<user_id>/<hash>.<ext>.enc`, with `_anon/`
for the pre-sign-in window. The `<user_id>` segment is the id the Delivery
Service returned from `verify-otp` — a server-chosen string on the **untrusted**
side of the security model — and `set_pin`, `unlock`, `logout` and
`delete_account` hand it to
`commands::r2::clear_media_cache(CacheScope::User(id))`, which empties that
directory with `remove_dir_all` per entry. `Path::join` with an absolute
component *replaces* the base and `..` walks out of it, so a DS answering
`{"user_id": "/home/alice"}` used to have the mandatory set-PIN step empty the
home directory.

Two layers stop it, and they are the same check so they cannot drift:

- **Chokepoint.** `commands::auth::accept_server_user_id` validates the id the
  moment it is decoded — `util::is_safe_id`: `[A-Za-z0-9_-]{1,64}`, one plain
  path component, so no separator, no `.`/`..`, no empty string. A malformed id
  ends the sign-in before anything is written to `accounts.json` or the
  keystore. It is a shape check rather than a strict ULID parse so accounts
  whose id predates the ULID scheme keep working.
- **At the join.** `r2::user_cache_dir(root, user)` is the only way an id
  becomes a cache path (`media_cache_dir`, `clear_media_cache`,
  `find_cached_file_for_user` all go through it) and returns `None` for
  anything `is_safe_id` rejects — "no such directory", which every caller
  already treats as "nothing there". `remove_dir_contents` and
  `enforce_cache_cap_to` additionally canonicalize their target and refuse to
  delete anywhere that is not the cache root (wipe-everything) or strictly
  inside it (per-user wipe, eviction), so a future caller building its own path
  cannot re-open the hole. `db::local::user_db_path` applies the same check to
  `pollis_<user_id>.db`.

`commands::r2::tests` drives `clear_media_cache` with `"/tmp/…"`, `"../…"`,
`""`, `".."` against a real root and checks a sibling directory outside it
survives; `commands::auth::server_user_id_is_not_a_path` runs `verify_otp`
against an in-process hostile DS and checks the sign-in ends with nothing
persisted (plus a positive control that a plain id gets past the check).

## The cache cap is enforced on writes, never on window focus (#930)

The cache is capped at 500 MB and evicted oldest-mtime-first
(`commands::r2::enforce_cache_cap`). **Every path that adds bytes calls it
immediately after its write** — attachments, emoji and public profile objects
alike — and that is the only thing that drives it.

It used to run on `WindowEvent::Focused(true)` as well, to catch files copied
into the directory from outside. That cost a full walk of the cache directory on
every alt-tab: work proportional to months of accumulated media rather than to
anything the user just did, and once #874 had removed the seven focus-time IPC
calls it was the only thing left happening on focus. A cache nobody is writing
to cannot grow past its cap, so the external-tamper case is still caught — at
the next write, which is the first moment it can matter.

There is deliberately **no public `enforce_cache_cap_now()`**; it existed only
as the focus hook's entry point. Two guards in `commands::r2`'s test module keep
it that way, and `cache_dir_walks()` (test builds only) counts directory walks
so "this path does not stat the whole cache" is assertable as a number rather
than a stopwatch. Note that CLAUDE.md's no-periodic-polling rule rules out the
obvious alternative: a timer is not the answer, cache mutation is.

## A DS-chosen download URL is origin-checked and size-capped

Every R2 access is a URL the **DS** picks (`POST /v1/r2/presign`) which the client
then fetches unauthenticated. Two things about that were taken on trust:

- **Where it points.** The presigned URL was used verbatim, so a compromised or
  impersonated DS could aim a `get` at attacker-controlled bytes or a `put` at
  an exfiltration endpoint. `commands::r2::PresignedUrl` is now the only thing
  the request builders accept, and its only constructor checks the URL's origin
  (`scheme://host[:port]`, userinfo discarded) against `config.r2_endpoint` and
  `config.r2_public_url`. An unconfigured build allows nothing.
- **How much it sends.** The body was read with `resp.bytes()` — fully resident —
  and only then measured against the caller's ceiling, so a URL that streams
  forever is an OOM kill that no downstream check ever reaches. `r2_get_url` now
  takes the cap as an argument, streams with `chunk()`, and returns `Ok(None)`
  the moment the cap would be exceeded, dropping the response mid-body. Caps:
  `MEDIA_CACHE_MAX_FILE_BYTES` for public objects and `download_file`,
  `EMOJI_MAX_BYTES` for emoji, `R2_MAX_DOWNLOAD_BYTES` (512 MiB) for attachment
  ciphertext, which is buffered whole to be decrypted and re-hashed.

The shared reqwest builder (`pollis_relay::http::http_client_builder`, which
`pollis-core` re-exports) also carries a 10s `connect_timeout` and a 30s
`read_timeout`. `reqwest`'s default is no timeout at all, so a peer that finished
the handshake and then went silent held the caller — and a pooled connection —
indefinitely. The read deadline is per-read inactivity rather than a whole-request
one on purpose: the same client fetches 100 MiB attachments, and a total deadline
generous enough for those over a slow link would not be a deadline.

Tests: `commands::r2::tests` (origin refusals including userinfo/suffix/scheme
tricks; a local stub that streams 64 MiB is abandoned at a 32 KiB cap, and a body
under its cap still arrives whole) and `pollis_relay::http::deadline_tests` (a
black-hole peer ends as a timeout instead of hanging).

## Latest-frame-wins, zero-copy frame fan-out (#480)

Decoded video frames reach every connected frame socket through
`media_server::FrameFanout`: **one slot per track per subscriber**. A producer
(the remote drain task, or a local pump's self-preview) replaces its track's slot
and wakes the subscriber; the subscriber's `pump_frames` loop takes every track's
newest frame, sends them one at a time, and only then takes the next batch. So a
renderer that falls behind sees each track's newest frame, never a queue of stale
ones, and at most one frame per track is ever waiting on a socket — the
semantics of a `tokio::sync::watch` per track, in a form a dynamic set of tracks
can be awaited as one. (A `broadcast::channel(8)` used to sit here: at 1080p30 its
eight slots plus the loopback socket buffers were a 250–500 ms reservoir
whenever the WebKit main thread was busy.) Every accepted socket also gets a
small `SO_SNDBUF` (`FrameListener`), so a stalled reader backpressures the pump
within a frame or two rather than 4 MB later.

Each send is **zero-copy**: the decoded I420 frame lives once behind the
`Arc<Vec<u8>>` shared across all subscribers, and
`Bytes::from_owner(SharedFrame(arc))` (axum 0.8) hands axum a `Bytes` that
*borrows* that shared buffer rather than memcpy-ing a full-resolution frame per
subscriber per frame. The `Arc` refcount — not a copy — is what fans the frame
out; the frame's memory frees exactly when the last subscriber's `Bytes` drops.

`SharedFrame` is a thin newtype wrapping the `Arc<Vec<u8>>` only because
`Bytes::from_owner` requires `AsRef<[u8]>`, which `Arc<Vec<u8>>` does not impl
directly (`AsRef<Vec<u8>>` yes, `AsRef<[u8]>` no).

Two process-wide relaxed atomic counters make the behaviour measurable:
`FRAMES_SENT` (frames handed to a socket, one per client per frame) and
`FRAMES_DROPPED` (frames replaced in a slot before a socket took them), read via
`frame_fanout_counters()`.

What arrives on the socket is also bounded at the source: a tile reports the
size it draws at and whether it is visible (`set_video_wants`, see
[commands.md](./commands.md)); a remote track nobody shows is paused at the SFU
and nothing is packed for it, and a track drawn small is packed small. On the
renderer side `screenShareSession` notifies the stats hook at most twice a
second and only when fps or dimensions changed, and `RemoteVideoTile` keeps one
WebGL context per canvas (freed, with `WEBGL_lose_context`, on unmount), a
backing store no larger than 2× its CSS box, and repacks padded strides.

## Mobile: no loopback server

A sandboxed React Native app cannot serve or fetch `http://127.0.0.1`
(App Transport Security, Android cleartext rules), so mobile takes the
file-path route instead: the uniffi bridge's `get_media_path(r2Key, contentHash, sha256, keyScope, destDir)`
decrypts the R2 object to `<cacheDirectory>/pollis-media/<content_hash>` (the file
id) and returns a
`file://` path; `mobile/lib/media/cache.ts` ref-counts it (`useMediaUri`).
The last release does not unlink at once: the file joins the
`MEDIA_RETAIN_MAX` (32) most recently released, so a tile that remounts — a
Vault Chat ↔ Media switch, a message row scrolled back into view — reads its
path synchronously (`peekMediaUri`) and `MediaImage` draws it on the first
frame with no blurhash and no fade. Before this every remount decrypted again
and played the placeholder (the hook started at `uri: null`, the file was
already unlinked, and `cachePolicy="none"` kept no decoded copy). The
bookkeeping is pure (`mobile/lib/media/store.ts`, `tests/media-store.test.ts`):
the oldest kept file beyond the bound is unlinked, a file in use never is, a
consumer that leaves mid-decrypt leaves the file kept rather than leaked, and
`trimMediaCache()` unlinks every kept file on lock and whenever the app goes to
the background (`startMediaRetention`, root layout). `MediaImage` uses
expo-image's **memory** cache only (never its disk cache); trims and sign-out
clear it with `Image.clearMemoryCache()`. Sign-out (`clearMediaCache`) still
wipes the directory and forgets everything.

The full-screen media viewer (`mobile/app/media.tsx`, #1248) adds a second,
NAMED copy of those same decrypted bytes under `pollis-media/named/<hash>/`
(`resolveNamedMediaUri` / `useNamedMediaUri`): the sender's filename plus an
extension, because AVPlayer (expo-video, expo-audio), the iOS photo library
and the share sheet all type a file by its extension and the hash file has
none. It is a local `copyAsync`, never a fetch, ref-counted and unlinked the
same way; Save to photos (expo-media-library, add-only) and Share
(expo-sharing) read from it.

The viewer's pager is virtualised for rolls of hundreds (#1492): only the page
on screen and one each side are mounted. An image page holds a `useMediaUri`
reference while mounted, so neighbours decrypt ahead of a swipe and a page that
scrolls further away drops its reference (the file is then kept, bounded, as
above); a video page takes its named copy only while on screen. The Vault's grid
is a virtualised list too, so a large vault never decrypts every tile at once,
and its cells are the chat's own `AttachmentTile` at a fixed square `box`, so a
picture has the same corners, frame, blurhash and sensitive cover in both views.

Desktop has the same rule: `getMediaUrl` already de-duplicated per object, and
`peekMediaUrl` (`frontend/src/services/r2-upload.ts`) answers an already
resolved URL synchronously, so `AttachmentDisplay` and the Vault's
`MediaGalleryView` tile start from it and a remount draws the image on its first
render. The gallery tile has the chat card's shape (0.5rem, no frame, blurhash
placeholder) in both skins; Playwright `e2e/vault-media-tiles.spec.ts`.

## A failed tile is never sticky (#1826)

A fetch that failed (network drop, a DS or R2 hiccup) used to leave its tile on
"Image unavailable" (mobile) or "failed to load" (desktop row and spotlight)
until it remounted, even after the same file loaded elsewhere. Recovery is
event-driven, never a timer or a poll:

- **The cache announces every success per content hash.** Mobile's store
  (`mobile/lib/media/store.ts`) calls `subscribe(hash)` listeners after each
  successful decrypt; desktop's `createMediaUrlCache`
  (`frontend/src/services/mediaUrlCache.ts`, behind `getMediaUrl` /
  `peekMediaUrl` / `subscribeMediaResolved` in `r2-upload.ts`) does the same
  after each `get_media_url`, and `announceMediaLoaded` adds an `<img>` /
  `<video>` that actually drew (the spotlight, a row), which also covers a URL
  that was already resolved.
- **Only a failed consumer listens, and only while failed.** Mobile's
  `startMediaTile` (`mobile/lib/media/tile.ts`, run by `useMediaUri` and
  `useNamedMediaUri`) subscribes on failure; the first announce releases the
  failed attempt's reference and re-acquires, which the cache answers at once
  from the file the other consumer holds. Every attempt is one acquire and one
  release with the generation it started in, so a sign-out clear still makes a
  stale release a no-op. Desktop's `AttachmentDisplay` and `MediaSpotlight`
  subscribe in an effect keyed on their `error` / `failed` state and take the
  announced URL. Unmount (and recovery) unsubscribes.
- **A remount asks again.** A failed fetch is dropped from the in-flight map on
  both platforms, so the next mount peeks (instant if the file is held or kept)
  and otherwise fetches afresh; nothing remembers the failure.

Tests: `mobile/tests/media-recovery.test.ts`, `frontend/tests/media-recovery.test.ts`
(a failed resolve, then a resolve elsewhere, leaves the tile rendered; a remount
re-asks). Device tier: Playwright `e2e/media-recovery.spec.ts` (the IPC mock's
`failMediaKeys` / `__tauriFailMedia` fail `get_media_url` for one key) and Maestro
`flows/media-recovery.yaml` (Android airplane mode; iOS has no network switch in
Maestro, so it covers the online render only).

---
_Back to [index.md](./index.md)_
