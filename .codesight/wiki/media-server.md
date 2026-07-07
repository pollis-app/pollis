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

This fits the "media is Rust-first" architecture (see [overview.md](./overview.md)):
the renderer's WebRTC is intentionally unused; IPC carries UI events only, never
media bytes.

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
