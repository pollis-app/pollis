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

/// Process-wide screenshare fan-out counters, so the zero-copy win (#480) is
/// measurable before/after without wiring anything through `AppState`.
///
/// `FRAMES_SENT` — decoded frames handed to a WS client's socket (one increment
/// per client per frame). `FRAMES_DROPPED` — frames a lagged/stalled webview
/// never received because the broadcast channel overwrote them before that
/// receiver caught up (latest-frame-wins; see `pump_frames`). Read them with
/// [`frame_fanout_counters`].
static FRAMES_SENT: AtomicU64 = AtomicU64::new(0);
static FRAMES_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the screenshare fan-out counters as `(frames_sent,
/// frames_dropped)`. Cheap relaxed loads — intended for a debug/metrics readout.
pub fn frame_fanout_counters() -> (u64, u64) {
    (
        FRAMES_SENT.load(Ordering::Relaxed),
        FRAMES_DROPPED.load(Ordering::Relaxed),
    )
}

/// Spawn the loopback media server on an OS-assigned port. Returns the
/// bound port so the caller can stash it in `AppState`. Server runs until
/// `AppState::shutdown()` is called (which fires `shutdown_signal`),
/// at which point axum's graceful-shutdown drains in-flight requests
/// and the accept loop returns — releasing its hold on the Tokio
/// runtime so the host process can exit.
///
/// Pre-#335 this task spawned with no shutdown path and pinned the
/// runtime alive forever, hanging the host process during auto-update. The
/// symptom was first hit under the (since-reverted, #386/#389) Electron
/// shell; the shutdown path here is what keeps the Tauri host able to exit.
pub async fn spawn(state: Arc<AppState>) -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let shutdown_signal = state.shutdown_signal.clone();
    let app = Router::new()
        .route("/{token}/{hash}", get(serve_media))
        // Decoded remote screenshare frames for the Tauri/WebKitGTK WebGL
        // render path (spike/tauri-revival). Token-gated like `serve_media`.
        // 3 segments so it can't collide with the `/{token}/{hash}` media route
        // (matchit panics on static-vs-param conflicts at the same position).
        .route("/ws/screenshare/{token}", get(ws_screenshare))
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

/// The loopback URL the renderer fetches `content_hash` from.
///
/// Lives beside the route it addresses so the two cannot disagree about the path
/// shape; `emoji::get_emoji_url` and `r2::get_media_url` each built it from their
/// own copy of this block.
///
/// Both the port and the token must be present. Without an active unlock the
/// server answers 403 anyway, so there is no point handing out a URL the caller
/// cannot use.
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

/// The loopback server's only gate: the caller's path token must equal the
/// secret minted at unlock, compared in constant time.
///
/// Every route runs it and there is one copy of it, so a route added later
/// cannot pick up a subtly different rule. A missing token (locked, or never
/// minted) is as much a `false` as a wrong one — callers collapse both to 403
/// rather than distinguishing them, so the server never reveals which check
/// failed.
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

    // Decrypt once into an owned buffer, then wrap it in `Bytes` so the Range
    // branch can hand back a sub-slice that SHARES this allocation instead of
    // copying the range out (`slice.to_vec()`). Range reads are the video-seek
    // path — warm during scrubbing, not per-frame, but the zero-copy slice is
    // free now that `Bytes` is in hand (#480 item 3). `Bytes::from(Vec)` takes
    // ownership without copying; the full-body path below moves it too.
    let plaintext: Bytes = match r2cmd::cache_decrypt(&file_bytes, &db_key, hash.as_bytes()) {
        Ok(p) => Bytes::from(p),
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
                    // Zero-copy: shares `plaintext`'s allocation, no memcpy.
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
                    // Header present but unparseable as a range we accept —
                    // fall through to the full-body 200 response.
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
/// media bytes becomes a `Response`.
///
/// Every exit of `serve_media` that carries or describes plaintext goes through
/// here, so the headers below cannot be attached to two of three paths: the
/// function takes the half-built `Builder`, and the `Body` never meets it
/// anywhere else.
fn media_response(builder: axum::http::response::Builder, body: Body) -> Response {
    let mut resp = builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    add_no_store_headers(resp.headers_mut());
    add_cors_headers(resp.headers_mut());
    resp
}

/// Keep decrypted bytes out of the webview's own on-disk HTTP cache.
///
/// A 200 with no cache directives at all is *storable by default* (RFC 9111
/// §3): it has a 200 status, a `GET` method and a `Content-Length`, and nothing
/// forbids storage — so WebKitGTK, WKWebView and WebView2 are each entitled to
/// write the response body into their own disk cache, and did. That cache is
/// the browser engine's, not ours: nothing in this app knows where it is, the
/// media-cache wipe on logout does not reach it, and its contents are the
/// plaintext this server exists to decrypt in memory.
///
/// `no-store` is the directive that forbids it, and it is the one that has to be
/// on the 206 range path too — video seeking is served entirely from there, so a
/// header attached only to the full-body 200 would leave every video the user
/// scrubbed sitting in the engine's cache.
///
/// `Pragma: no-cache` rides along for HTTP/1.0-era caches that predate
/// `Cache-Control`. It is redundant for every engine this app ships against and
/// costs 18 bytes a response; it is here because the cost of being wrong about
/// which intermediary is in the path is a plaintext copy on someone's disk.
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
    if !token_ok(&state, &token).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let rx = state.screenshare_frame_tx.subscribe();
    ws.on_upgrade(move |socket| pump_frames(socket, rx))
}

/// Owns a shared decoded frame so a [`Bytes`] can borrow its bytes without
/// copying them out of the `Arc`. `Arc<Vec<u8>>` impls `AsRef<Vec<u8>>` but not
/// `AsRef<[u8]>` (which `Bytes::from_owner` requires), so this thin newtype
/// bridges the two. Dropping the last `Bytes` clone drops this, decrementing the
/// `Arc` — the frame's memory frees exactly when no subscriber references it.
struct SharedFrame(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedFrame {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Forward broadcast frames to one WebSocket client until either side
/// closes. Lagged receivers (a stalled webview) drop the oldest frames
/// rather than back-pressuring the decoder — latest-frame-wins.
///
/// Each frame is sent ZERO-COPY: the decoded I420 frame lives once behind an
/// `Arc<Vec<u8>>` shared across every subscriber, and `Bytes::from_owner` hands
/// axum a `Bytes` that borrows that shared buffer rather than memcpy-ing a
/// full-res frame per client per frame (#480). The `Arc` refcount — not a copy —
/// is what fans the frame out.
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
            // The webview fell behind and the channel overwrote `skipped` frames
            // before this receiver read them. Count them as dropped and keep
            // going from the newest frame — never back-pressure the decoder.
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

    // The whole point of #480: sending a frame to a subscriber must NOT copy the
    // bytes out of the shared `Arc`. `Bytes::from_owner(SharedFrame(arc))` has to
    // borrow the Arc's buffer in place — proven here by the payload pointing at
    // the exact same address as the Arc's data.
    #[test]
    fn shared_frame_send_is_zero_copy() {
        let frame = Arc::new(vec![1u8, 2, 3, 4, 5]);
        let src_ptr = frame.as_ptr();

        let payload = Bytes::from_owner(SharedFrame(Arc::clone(&frame)));

        // Same contents...
        assert_eq!(payload.as_ref(), &[1, 2, 3, 4, 5]);
        // ...and, crucially, the SAME backing memory — no memcpy happened.
        assert_eq!(payload.as_ptr(), src_ptr);
    }

    // The Arc must stay alive exactly as long as some subscriber's `Bytes` still
    // references it, and free once the last one drops — no leak, no early free.
    #[test]
    fn shared_frame_holds_and_releases_the_arc() {
        let frame = Arc::new(vec![9u8; 32]);
        assert_eq!(Arc::strong_count(&frame), 1);

        let payload = Bytes::from_owner(SharedFrame(Arc::clone(&frame)));
        // The owner inside `payload` keeps the Arc alive.
        assert_eq!(Arc::strong_count(&frame), 2);

        // A cheap clone (a second WS client) shares the same buffer, not a copy.
        let payload2 = payload.clone();
        assert_eq!(payload2.as_ptr(), payload.as_ptr());

        drop(payload);
        drop(payload2);
        // Every borrower gone → the Arc is back to just our test handle.
        assert_eq!(Arc::strong_count(&frame), 1);
    }

    #[test]
    fn fanout_counters_are_readable() {
        // Relaxed snapshot; monotonic in the running server. Just prove the
        // accessor wires to the two atomics without panicking.
        let (sent, dropped) = frame_fanout_counters();
        let _ = (sent, dropped);
    }

    // ── Cache directives (#1000) ───────────────────────────────────────────

    /// Bring up the real server over a real cached file and return
    /// `(base_url, content_hash)`.
    ///
    /// Everything here is the shipped path: `cache_encrypt` writes the file,
    /// `find_cached_file` locates it, `serve_media` decrypts it, and the
    /// assertions read the headers off an actual HTTP response — so a response
    /// builder that stops attaching a header fails this test rather than
    /// passing a unit check on a helper nothing calls.
    async fn serving_one_cached_file() -> (String, String) {
        use crate::commands::pin::UnlockState;

        const USER: &str = "media-server-headers";
        let db_key = vec![7u8; 32];
        // 64 hex chars — `serve_media` rejects anything else before it touches
        // the filesystem.
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

    /// The webview must not be allowed to keep a copy.
    ///
    /// A 200 carrying `Content-Type` and `Content-Length` and no cache
    /// directives is storable by default (RFC 9111 §3), so WebKitGTK, WKWebView
    /// and WebView2 each wrote decrypted images, audio and video into their own
    /// on-disk cache — one nothing in this app clears, not even the media-cache
    /// wipe on logout.
    ///
    /// Both paths, in one test on purpose: the 206 is the one a video seek
    /// takes, and it is the one a response-builder refactor is most likely to
    /// leave behind.
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

        // The 416 carries no body, but it does describe one; it goes through
        // the same funnel and is checked here so the funnel has no exceptions.
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

    /// The structural half of the guarantee: `serve_media` does not build a
    /// response body itself, so a new exit path cannot ship without the
    /// directives. Every body it returns is constructed by `media_response`,
    /// which is the only place the headers are attached.
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
