# Loopback Media Server

A small axum HTTP server bound to `127.0.0.1:<os-assigned-port>`
(`pollis-core/src/media_server.rs`), spawned at startup and shut down via
`AppState::shutdown()`'s graceful-drain signal. It is how large/decoded media
bytes reach the WebView **without** riding Tauri's IPC `Channel` — per-frame JS
dispatch stalls on V8 GC, so bulk media is served over loopback instead. Two
token-gated routes (same secret, 403 on anything else):

- **`GET /{token}/{hash}`** (`serve_media`) — the decrypted bytes of a cached,
  content-addressed media file, honouring single-range `Range` requests. The
  decrypted plaintext is handed out as a `Bytes` (`Bytes::from(Vec)`), so a range
  slice is a zero-copy view into it.
- **`GET /ws/screenshare/{token}`** (`ws_screenshare`) — a WebSocket that streams
  **decoded remote screenshare frames** (packed I420, the `pack_frame_bytes` wire
  format) as binary messages. The renderer uploads the Y/U/V planes into a WebGL
  YUV→RGB shader — the transport the `rustwebrtc` PoC proved sustains 1080p60+
  where per-frame Tauri IPC `Channel` dispatch stalled on V8 GC (#305 Phase 1).
  One frame stream serves every track; the renderer dispatches by the `track_key`
  in each frame header.

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

This fits the "media is Rust-first" architecture (see [overview.md](./overview.md)):
the renderer's WebRTC is intentionally unused; IPC carries UI events only, never
media bytes.

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
side of the security model — and `set_pin`, `unlock` and `logout` hand it to
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

## Zero-copy screenshare frame fan-out (#480)

Decoded screenshare frames are fanned out to every connected WebView subscriber
over a `tokio::sync::broadcast` channel of `Arc<Vec<u8>>`. Each subscriber's
`pump_frames` loop forwards a frame **zero-copy**: the decoded I420 frame lives
once behind the `Arc<Vec<u8>>` shared across all subscribers, and
`Bytes::from_owner(SharedFrame(arc))` (axum 0.8) hands axum a `Bytes` that
*borrows* that shared buffer rather than memcpy-ing a full-resolution frame per
subscriber per frame. The `Arc` refcount — not a copy — is what fans the frame
out; the frame's memory frees exactly when the last subscriber's `Bytes` drops.

`SharedFrame` is a thin newtype wrapping the `Arc<Vec<u8>>` only because
`Bytes::from_owner` requires `AsRef<[u8]>`, which `Arc<Vec<u8>>` does not impl
directly (`AsRef<Vec<u8>>` yes, `AsRef<[u8]>` no).

Lagged receivers (a stalled WebView) drop the oldest frames rather than
back-pressuring the decoder — latest-frame-wins. Two process-wide relaxed atomic
counters make the win measurable: `FRAMES_SENT` (frames handed to a socket, one
per client per frame) and `FRAMES_DROPPED` (frames a lagged receiver never got),
read via `frame_fanout_counters()`.

---
_Back to [index.md](./index.md)_
