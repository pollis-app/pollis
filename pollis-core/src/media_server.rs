//! Local-loopback HTTP server for cached media.
//!
//! Every media element loads `http://127.0.0.1:<port>/<token>/<hash>`; the
//! server decrypts the cached file under `db_key` and streams it with real
//! HTTP Range. The per-session token (rotated on unlock, cleared on logout) is
//! the protection, not origin checks. `asset://` fails for `<audio>/<video>` on
//! WebKitGTK and IPC blobs blow the V8 heap; see `.codesight/wiki/media-server.md`.

use std::sync::atomic::{AtomicU64, Ordering};
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
use bytes::Bytes;

use crate::commands::r2 as r2cmd;
use crate::state::AppState;

/// Screenshare fan-out counters (#480): frames sent per client, and frames a
/// lagged webview never received (latest-frame-wins, see `pump_frames`).
static FRAMES_SENT: AtomicU64 = AtomicU64::new(0);
static FRAMES_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the screenshare fan-out counters as `(frames_sent, frames_dropped)`.
pub fn frame_fanout_counters() -> (u64, u64) {
    (
        FRAMES_SENT.load(Ordering::Relaxed),
        FRAMES_DROPPED.load(Ordering::Relaxed),
    )
}

/// Spawn the loopback media server on an OS-assigned port and return the port.
/// It runs until `AppState::shutdown()` fires `shutdown_signal`, so it never
/// pins the runtime and blocks host exit (#335).
pub async fn spawn(state: Arc<AppState>) -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let shutdown_signal = state.shutdown_signal.clone();
    let app = Router::new()
        .route("/{token}/{hash}", get(serve_media))
        // Decoded remote screenshare frames for the WebGL render path. Three
        // segments so matchit cannot conflict with `/{token}/{hash}`.
        .route("/ws/screenshare/{token}", get(ws_screenshare))
        .with_state(state);

    tokio::spawn(async move {
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_signal.notified().await;
            })
            .await;
        if let Err(e) = result {
            crate::diag!("[media_server] axum::serve exited: {e}");
        }
    });

    Ok(port)
}

/// The loopback URL the renderer fetches `content_hash` from. Errors unless
/// both the port and the token are set.
pub(crate) async fn loopback_url(
    state: &Arc<AppState>,
    content_hash: &str,
) -> crate::error::Result<String> {
    use crate::error::Error;
    let port = state
        .media_server_port
        .lock()
        .await
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media server not started")))?;
    let token = state
        .media_server_token
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media server token not set; not unlocked")))?;
    Ok(format!("http://127.0.0.1:{port}/{token}/{content_hash}"))
}

/// The loopback server's only gate: the path token must equal the secret minted
/// at unlock, in constant time. Every route runs it; a missing token is as much
/// a `false` as a wrong one, so callers cannot reveal which check failed.
async fn token_ok(state: &Arc<AppState>, token: &str) -> bool {
    let Some(expected) = state.media_server_token.lock().await.clone() else {
        return false;
    };
    constant_time_eq(token.as_bytes(), expected.as_bytes())
}

/// `GET /<token>/<hash>` — serves the decrypted bytes for a cached
/// content-addressed media file. Honours single-range `Range` requests.
async fn serve_media(
    State(state): State<Arc<AppState>>,
    Path((token, hash)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !token_ok(&state, &token).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    // 64 hex chars, or we don't touch the FS.
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let db_key = match state.unlock.lock().await.as_ref() {
        Some(u) => u.db_key.to_vec(),
        None => return StatusCode::FORBIDDEN.into_response(),
    };

    // The cache stores `<hash>.<ext>.enc`; the extension sets Content-Type.
    let (path, ext) = match r2cmd::find_cached_file(&hash) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let file_bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    // `Bytes` so the Range branch can return a zero-copy sub-slice (#480).
    let plaintext: Bytes = match r2cmd::cache_decrypt(&file_bytes, &db_key, hash.as_bytes()) {
        Ok(p) => Bytes::from(p),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let content_type = r2cmd::content_type_for_ext(&ext);
    let total_len = plaintext.len() as u64;

    // Single range only; players never request multi-range for media.
    if let Some(range_hdr) = headers.get(header::RANGE) {
        if let Ok(range_str) = range_hdr.to_str() {
            match parse_single_range(range_str, total_len) {
                Ok(Some((start, end))) => {
                    let slice = plaintext.slice(start as usize..=end as usize);
                    let len = end - start + 1;
                    return media_response(
                        Response::builder()
                            .status(StatusCode::PARTIAL_CONTENT)
                            .header(header::CONTENT_TYPE, content_type)
                            .header(header::CONTENT_LENGTH, len.to_string())
                            .header(
                                header::CONTENT_RANGE,
                                format!("bytes {start}-{end}/{total_len}"),
                            )
                            .header(header::ACCEPT_RANGES, "bytes"),
                        Body::from(slice),
                    );
                }
                Ok(None) => {
                    // Unparseable range: fall through to the full 200.
                }
                Err(()) => {
                    return media_response(
                        Response::builder()
                            .status(StatusCode::RANGE_NOT_SATISFIABLE)
                            .header(header::CONTENT_RANGE, format!("bytes */{total_len}")),
                        Body::empty(),
                    );
                }
            }
        }
    }

    media_response(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, total_len.to_string())
            .header(header::ACCEPT_RANGES, "bytes"),
        Body::from(plaintext),
    )
}

/// Finish a response from `serve_media` — the single place a body of decrypted
///
/// Every exit of `serve_media` goes through here, so the no-store headers
/// cannot be attached to only some paths (enforced by a source test below).
fn media_response(builder: axum::http::response::Builder, body: Body) -> Response {
    let mut resp = builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    add_no_store_headers(resp.headers_mut());
    add_cors_headers(resp.headers_mut());
    resp
}

/// Keep decrypted bytes out of the webview's own on-disk HTTP cache (#1000).
///
/// A 200 with no cache directives is storable by default (RFC 9111 §3), and
/// that engine cache is beyond the logout wipe. `no-store` must be on the 206
/// path too, since video seeking is served entirely from it. `Pragma` covers
/// HTTP/1.0-era caches.
fn add_no_store_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
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
/// `Ok(Some((start, end)))`: satisfiable, end inclusive. `Ok(None)`: malformed,
/// fall through to a full 200 (RFC 9110 §14.1.1). `Err(())`: unsatisfiable → 416.
fn parse_single_range(range_str: &str, total: u64) -> Result<Option<(u64, u64)>, ()> {
    let rest = match range_str.strip_prefix("bytes=") {
        Some(r) => r,
        None => return Ok(None),
    };
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

/// `GET /ws/screenshare/<token>`: a WebSocket streaming decoded remote
/// screenshare frames (packed I420, `pack_frame_bytes` format) for the WebGL
/// renderer, which dispatches by each frame's `track_key`. Per-frame Tauri IPC
/// stalled on V8 GC (#305).
async fn ws_screenshare(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if !token_ok(&state, &token).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rx = state.screenshare_frame_tx.subscribe();
    ws.on_upgrade(move |socket| pump_frames(socket, rx))
}

/// Lets `Bytes::from_owner` borrow a shared frame without copying:
/// `Arc<Vec<u8>>` is not `AsRef<[u8]>`.
struct SharedFrame(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedFrame {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Forward broadcast frames to one WebSocket client until either side closes.
/// Zero-copy per client (#480); a lagged receiver drops the oldest frames
/// rather than back-pressuring the decoder.
async fn pump_frames(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<Arc<Vec<u8>>>,
) {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match rx.recv().await {
            Ok(frame) => {
                let payload = Bytes::from_owner(SharedFrame(frame));
                if socket.send(Message::Binary(payload)).await.is_err() {
                    break;
                }
                FRAMES_SENT.fetch_add(1, Ordering::Relaxed);
            }
            Err(RecvError::Lagged(skipped)) => {
                FRAMES_DROPPED.fetch_add(skipped, Ordering::Relaxed);
                continue;
            }
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

/// Generate a fresh 32-byte hex token; the unlock paths rotate it.
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

    // #480: the payload must borrow the Arc's buffer in place.
    #[test]
    fn shared_frame_send_is_zero_copy() {
        let frame = Arc::new(vec![1u8, 2, 3, 4, 5]);
        let src_ptr = frame.as_ptr();

        let payload = Bytes::from_owner(SharedFrame(Arc::clone(&frame)));

        assert_eq!(payload.as_ref(), &[1, 2, 3, 4, 5]);
        assert_eq!(payload.as_ptr(), src_ptr);
    }

    // The Arc lives while any `Bytes` references it, and frees after the last.
    #[test]
    fn shared_frame_holds_and_releases_the_arc() {
        let frame = Arc::new(vec![9u8; 32]);
        assert_eq!(Arc::strong_count(&frame), 1);

        let payload = Bytes::from_owner(SharedFrame(Arc::clone(&frame)));
        assert_eq!(Arc::strong_count(&frame), 2);

        let payload2 = payload.clone();
        assert_eq!(payload2.as_ptr(), payload.as_ptr());

        drop(payload);
        drop(payload2);
        assert_eq!(Arc::strong_count(&frame), 1);
    }

    #[test]
    fn fanout_counters_are_readable() {
        let (sent, dropped) = frame_fanout_counters();
        let _ = (sent, dropped);
    }

    /// Bring up the real server over a real cached file and return
    /// `(base_url, content_hash)`, so header assertions read an actual response.
    async fn serving_one_cached_file() -> (String, String) {
        use crate::commands::pin::UnlockState;

        const USER: &str = "media-server-headers";
        let db_key = vec![7u8; 32];
        let content_hash = "b".repeat(64);

        let state = Arc::new(AppState::new_with_parts(
            crate::config::Config::for_test().expect("test config"),
            Arc::new(crate::keystore::InMemoryKeystore::new()),
        ));
        *state.unlock.lock().await = Some(UnlockState {
            user_id: USER.to_string(),
            db_key: zeroize::Zeroizing::new(db_key.clone()),
            account_id_key: zeroize::Zeroizing::new(vec![9u8; 32]),
        });
        let token = fresh_token();
        *state.media_server_token.lock().await = Some(token.clone());

        let dir = r2cmd::test_cache_root().join(USER);
        crate::private_fs::create_dir_all(&dir).expect("cache dir");
        r2cmd::set_cache_user(Some(USER));
        let plaintext = vec![0xABu8; 4096];
        let encrypted = r2cmd::cache_encrypt(plaintext, &db_key, content_hash.as_bytes())
            .expect("cache encrypt");
        std::fs::write(dir.join(format!("{content_hash}.png.enc")), &encrypted)
            .expect("write cache file");

        let port = spawn(state).await.expect("spawn media server");
        (format!("http://127.0.0.1:{port}/{token}"), content_hash)
    }

    /// The webview must not be allowed to keep a copy (#1000). Covers 200, the
    /// 206 a video seek takes, and the bodyless 416.
    #[tokio::test]
    async fn decrypted_bytes_are_never_storable_by_the_webview() {
        let (base, hash) = serving_one_cached_file().await;
        let client = reqwest::Client::new();

        let full = client
            .get(format!("{base}/{hash}"))
            .send()
            .await
            .expect("full-body request");
        assert_eq!(full.status().as_u16(), 200);
        assert_no_store("200", full.headers());
        assert_eq!(full.bytes().await.expect("body").len(), 4096);

        let ranged = client
            .get(format!("{base}/{hash}"))
            .header(reqwest::header::RANGE, "bytes=0-127")
            .send()
            .await
            .expect("range request");
        assert_eq!(
            ranged.status().as_u16(),
            206,
            "the fixture must actually exercise the range path"
        );
        assert_no_store("206", ranged.headers());
        assert_eq!(ranged.bytes().await.expect("body").len(), 128);

        let unsatisfiable = client
            .get(format!("{base}/{hash}"))
            .header(reqwest::header::RANGE, "bytes=99999-")
            .send()
            .await
            .expect("unsatisfiable range request");
        assert_eq!(unsatisfiable.status().as_u16(), 416);
        assert_no_store("416", unsatisfiable.headers());
    }

    fn assert_no_store(which: &str, headers: &reqwest::header::HeaderMap) {
        let cache_control = headers
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            cache_control.contains("no-store"),
            "the {which} response is storable: Cache-Control was {cache_control:?}"
        );
        assert_eq!(
            headers
                .get(reqwest::header::PRAGMA)
                .and_then(|v| v.to_str().ok()),
            Some("no-cache"),
            "the {which} response lost its Pragma fallback"
        );
    }

    /// `serve_media` never builds a body itself, so every exit goes through
    /// `media_response` and carries the directives.
    #[test]
    fn serve_media_never_builds_a_response_body_itself() {
        let source = include_str!("media_server.rs");
        let start = source
            .find("async fn serve_media(")
            .expect("serve_media must exist");
        let end = source[start..]
            .find("\n/// Finish a response from `serve_media`")
            .expect("media_response must follow serve_media");
        let body = &source[start..start + end];
        assert!(
            !body.contains(".body("),
            "`serve_media` builds a response body directly. Every response it returns carries \
             (or describes) decrypted media bytes, and `media_response` is what attaches \
             `Cache-Control: no-store` to them — a body built anywhere else is a path the \
             webview may cache to disk."
        );
    }
}
