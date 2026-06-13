//! Local-loopback HTTP server for cached media.
//!
//! The webview renders every `<img>/<audio>/<video>` element via
//! `src="http://127.0.0.1:<port>/<token>/<hash>"`. The server reads the
//! AES-GCM-encrypted file from the on-disk media cache, decrypts under
//! the per-process `db_key`, and streams plaintext bytes back. Only
//! callers presenting the per-session token (rotated on unlock, cleared
//! on logout) can access bytes — origin checks are not the protection
//! here, the token is.
//!
//! Why a server (rather than asset:// or IPC bytes):
//! * `asset://` works for `<img>` but Linux WebKitGTK's GStreamer source
//!   plugin rejects it for `<audio>/<video>` (MEDIA_ERR_SRC_NOT_SUPPORTED).
//! * IPC bytes → `Blob` URL works for media elements but blows V8's heap
//!   on multi-MB files and can't do real HTTP Range, so seeking large
//!   videos stalls.
//! * A real HTTP server gives proper Range, one URL pattern across
//!   image/audio/video, and keeps decryption in Rust where the keys
//!   already live.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::commands::r2 as r2cmd;
use crate::state::AppState;

/// Spawn the loopback media server on an OS-assigned port. Returns the
/// bound port so the caller can stash it in `AppState`. Server runs until
/// `AppState::shutdown()` is called (which fires `shutdown_signal`),
/// at which point axum's graceful-shutdown drains in-flight requests
/// and the accept loop returns — releasing its hold on the Tokio
/// runtime so the host process can exit.
///
/// Pre-#335 this task spawned with no shutdown path and pinned the
/// runtime alive forever, causing Squirrel.Mac's ShipIt to hang during
/// auto-update; see `electron/src/main.ts`'s graceful-quit handlers.
pub async fn spawn(state: Arc<AppState>) -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let shutdown_signal = state.shutdown_signal.clone();
    let app = Router::new()
        .route("/:token/:hash", get(serve_media))
        // Decoded remote screenshare frames for the Tauri/WebKitGTK WebGL
        // render path (spike/tauri-revival). Token-gated like `serve_media`.
        // 3 segments so it can't collide with the `/:token/:hash` media route
        // (matchit panics on static-vs-param conflicts at the same position).
        .route("/ws/screenshare/:token", get(ws_screenshare))
        .with_state(state);

    tokio::spawn(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_signal.notified().await;
            })
            .await;
        if let Err(e) = result {
            eprintln!("[media_server] axum::serve exited: {e}");
        }
    });

    Ok(port)
}

/// `GET /<token>/<hash>` — serves the decrypted bytes for a cached
/// content-addressed media file. Honours single-range `Range` requests.
async fn serve_media(
    State(state): State<Arc<AppState>>,
    Path((token, hash)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    // Token gate. Mismatch / missing / locked all collapse to 403 so the
    // server gives no information about which check failed.
    let expected = state.media_server_token.lock().await.clone();
    let expected = match expected {
        Some(t) => t,
        None => return StatusCode::FORBIDDEN.into_response(),
    };
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Hash shape sanity. 64 ASCII hex chars — anything else can't be a
    // real content_hash and we don't want to touch the FS for it.
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // db_key gate — without an active unlock the per-file key can't be
    // derived, so we can't decrypt anything anyway.
    let db_key = match state.unlock.lock().await.as_ref() {
        Some(u) => u.db_key.to_vec(),
        None => return StatusCode::FORBIDDEN.into_response(),
    };

    // Look the file up by hash. The cache stores `<hash>.<ext>.enc`; the
    // extension is decorative (we already know the bytes from the hash)
    // but we use it to set Content-Type.
    let (path, ext) = match r2cmd::find_cached_file(&hash) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let file_bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let plaintext = match r2cmd::cache_decrypt(&file_bytes, &db_key, hash.as_bytes()) {
        Ok(p) => p,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let content_type = r2cmd::content_type_for_ext(&ext);
    let total_len = plaintext.len() as u64;

    // Range parsing — single range only. Multi-range responses are a
    // mess we don't need; browsers/players never request them for
    // `<audio>/<video>`.
    if let Some(range_hdr) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_hdr.to_str() {
            match parse_single_range(range_str, total_len) {
                Ok(Some((start, end))) => {
                    let slice = &plaintext[start as usize..=end as usize];
                    let len = end - start + 1;
                    let mut resp = Response::builder()
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_TYPE, content_type)
                        .header(header::CONTENT_LENGTH, len.to_string())
                        .header(
                            header::CONTENT_RANGE,
                            format!("bytes {start}-{end}/{total_len}"),
                        )
                        .header(header::ACCEPT_RANGES, "bytes")
                        .body(Body::from(slice.to_vec()))
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    add_cors_headers(resp.headers_mut());
                    return resp;
                }
                Ok(None) => {
                    // Header present but unparseable as a range we accept —
                    // fall through to the full-body 200 response.
                }
                Err(()) => {
                    let mut resp = Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{total_len}"))
                        .body(Body::empty())
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    add_cors_headers(resp.headers_mut());
                    return resp;
                }
            }
        }
    }

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, total_len.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .body(Body::from(plaintext))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    add_cors_headers(resp.headers_mut());
    resp
}

fn add_cors_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Range, Content-Length, Accept-Ranges"),
    );
}

/// Parse `bytes=START-END` / `bytes=START-` / `bytes=-SUFFIX`.
///
/// `Ok(Some((start, end)))` — a satisfiable range, end inclusive.
/// `Ok(None)` — header was malformed in a way that should fall through
///   to a full 200 (per RFC 9110 §14.1.1).
/// `Err(())` — range is syntactically valid but unsatisfiable → 416.
fn parse_single_range(range_str: &str, total: u64) -> Result<Option<(u64, u64)>, ()> {
    let rest = match range_str.strip_prefix("bytes=") {
        Some(r) => r,
        None => return Ok(None),
    };
    // Reject multi-range explicitly.
    if rest.contains(',') {
        return Ok(None);
    }
    let (start_s, end_s) = match rest.split_once('-') {
        Some(p) => p,
        None => return Ok(None),
    };
    if total == 0 {
        return Err(());
    }
    let last = total - 1;

    if start_s.is_empty() {
        // Suffix range: bytes=-N
        let n: u64 = match end_s.parse() {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        if n == 0 {
            return Err(());
        }
        let start = total.saturating_sub(n);
        return Ok(Some((start, last)));
    }

    let start: u64 = match start_s.parse() {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let end: u64 = if end_s.is_empty() {
        last
    } else {
        match end_s.parse() {
            Ok(e) => e,
            Err(_) => return Ok(None),
        }
    };
    if start > end || start > last {
        return Err(());
    }
    let end = end.min(last);
    Ok(Some((start, end)))
}

/// `GET /ws/screenshare/<token>` — upgrades to a WebSocket that streams
/// decoded remote screenshare frames (packed I420, the `pack_frame_bytes`
/// wire format) as binary messages. This is the Tauri/WebKitGTK render
/// transport (spike/tauri-revival): the renderer parses each message and
/// uploads the Y/U/V planes into a WebGL YUV→RGB shader — the path the
/// `rustwebrtc` PoC proved sustains 1080p60+ where per-frame Tauri IPC
/// `Channel` dispatch stalled on V8 GC (#305 Phase 1).
///
/// One frame stream serves every track; the renderer dispatches by the
/// `track_key` carried in each frame's header, exactly as the legacy
/// `Channel` path did.
async fn ws_screenshare(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    // Token gate — same secret as the media route, same 403-on-anything.
    let expected = state.media_server_token.lock().await.clone();
    let expected = match expected {
        Some(t) => t,
        None => return StatusCode::FORBIDDEN.into_response(),
    };
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rx = state.screenshare_frame_tx.subscribe();
    ws.on_upgrade(move |socket| pump_frames(socket, rx))
}

/// Forward broadcast frames to one WebSocket client until either side
/// closes. Lagged receivers (a stalled webview) drop the oldest frames
/// rather than back-pressuring the decoder — latest-frame-wins.
async fn pump_frames(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<Arc<Vec<u8>>>,
) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(frame) => {
                if socket.send(Message::Binary(frame.to_vec())).await.is_err() {
                    break;
                }
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Generate a fresh 32-byte hex token. Called from the unlock paths to
/// rotate the per-session secret.
pub fn fresh_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_full_open_ended() {
        assert_eq!(parse_single_range("bytes=0-", 100), Ok(Some((0, 99))));
    }

    #[test]
    fn range_suffix() {
        assert_eq!(parse_single_range("bytes=-10", 100), Ok(Some((90, 99))));
        assert_eq!(parse_single_range("bytes=-200", 100), Ok(Some((0, 99))));
    }

    #[test]
    fn range_explicit() {
        assert_eq!(parse_single_range("bytes=10-19", 100), Ok(Some((10, 19))));
        assert_eq!(parse_single_range("bytes=10-200", 100), Ok(Some((10, 99))));
    }

    #[test]
    fn range_unsatisfiable() {
        assert_eq!(parse_single_range("bytes=200-300", 100), Err(()));
        assert_eq!(parse_single_range("bytes=50-10", 100), Err(()));
    }

    #[test]
    fn range_multi_falls_through() {
        assert_eq!(
            parse_single_range("bytes=0-10,20-30", 100),
            Ok(None)
        );
    }

    #[test]
    fn token_constant_time() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
