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

Three subsystems write into the one content-addressed cache and are served by
the same `GET /{token}/{hash}` route without it knowing which: message
attachments (`commands::r2::get_media_url`), custom emoji
(`commands::emoji::get_emoji_url`), and — since #874 — public profile objects,
i.e. avatars and group icons (`commands::r2::get_public_file_url`). The route
resolves a hash by scanning for `<hash>.<ext>.enc`, so a new producer needs no
server change at all; what it needs is a content-addressed name, which is why
avatars had to stop living at a mutable `avatars/{user_id}` key first.

This fits the "media is Rust-first" architecture (see [overview.md](./overview.md)):
the renderer's WebRTC is intentionally unused; IPC carries UI events only, never
media bytes.

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
