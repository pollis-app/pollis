//! Local-loopback HTTP server for cached media.
//!
//! The webview loads media from `http://127.0.0.1:<port>/<token>/<hash>`; the per-session token, not origin checks, is the protection.
//!
//! A server because `asset://` fails for WebKitGTK `<audio>/<video>` and IPC blobs can't do HTTP Range.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
use pollis_device_cert::constant_time_eq;

/// Screenshare fan-out counters (#480), read via [`frame_fanout_counters`].
static FRAMES_SENT: AtomicU64 = AtomicU64::new(0);
static FRAMES_DROPPED: AtomicU64 = AtomicU64::new(0);

/// `(frames_sent, frames_dropped)`, relaxed loads.
pub fn frame_fanout_counters() -> (u64, u64) {
    (
        FRAMES_SENT.load(Ordering::Relaxed),
        FRAMES_DROPPED.load(Ordering::Relaxed),
    )
}

/// Send buffer for every accepted loopback socket. Frames are big (3 MB at 1080p) and the
/// pump sends one at a time; a small buffer means a stalled renderer backpressures the
/// pump within a frame or two instead of a tunable 4 MB behind it.
const FRAME_SOCKET_SNDBUF: usize = 256 * 1024;

/// The media server's listener: a `TcpListener` whose accepted sockets get a small
/// `SO_SNDBUF` (see [`FRAME_SOCKET_SNDBUF`]).
struct FrameListener(tokio::net::TcpListener);

impl axum::serve::Listener for FrameListener {
    type Io = tokio::net::TcpStream;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, addr)) => {
                    if let Err(e) =
                        socket2::SockRef::from(&stream).set_send_buffer_size(FRAME_SOCKET_SNDBUF)
                    {
                        crate::diag!("[media_server] SO_SNDBUF: {e}");
                    }
                    return (stream, addr);
                }
                Err(e) => {
                    crate::diag!("[media_server] accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

/// Spawn the loopback media server on an OS-assigned port; returns it.
///
/// `AppState::shutdown()` drains it gracefully so the host process can exit (#335).
pub async fn spawn(state: Arc<AppState>) -> std::io::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let listener = FrameListener(listener);

    let shutdown_signal = state.shutdown_signal.clone();
    let app = Router::new()
        .route("/{token}/{hash}", get(serve_media))
        // Decoded screenshare frames for the WebGL path. 3 segments so matchit can't collide with `/{token}/{hash}`.
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

/// The loopback URL the renderer fetches `content_hash` from.
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
/// One copy for every route. A missing token is a `false` too; both collapse to 403.
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

    // 64 hex chars, or we never touch the FS.
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let db_key = match state.unlock.lock().await.as_ref() {
        Some(u) => u.db_key.to_vec(),
        None => return StatusCode::FORBIDDEN.into_response(),
    };

    // The cache stores `<hash>.<ext>.enc`; the ext only sets Content-Type.
    let (path, ext) = match r2cmd::find_cached_file(&hash) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let file_bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    // `Bytes` so the Range branch returns a zero-copy sub-slice (#480).
    let plaintext: Bytes = match r2cmd::cache_decrypt(&file_bytes, &db_key, hash.as_bytes()) {
        Ok(p) => Bytes::from(p),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let content_type = r2cmd::content_type_for_ext(&ext);
    let total_len = plaintext.len() as u64;

    // Single range only; players never request multi-range.
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
                    // Unparseable: fall through to the full-body 200.
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
/// A 200 without cache directives is storable (RFC 9111 §3), and webview engines did cache decrypted media to disk.
///
/// `no-store` must be on the 206 range path too: video seeking is served from it.
///
/// `Pragma: no-cache` for HTTP/1.0-era caches, in case one is in the path.
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

/// `GET /ws/screenshare/<token>`: WebSocket of decoded I420 frames for the WebGL renderer (#305).
///
/// One stream serves every track; the renderer dispatches by `track_key`.
async fn ws_screenshare(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if !token_ok(&state, &token).await {
        return StatusCode::FORBIDDEN.into_response();
    }

    let fanout = Arc::clone(&state.screenshare_frame_tx);
    ws.on_upgrade(move |socket| pump_frames(socket, fanout))
}

/// Latest-frame-wins fan-out of packed video frames to the frame sockets.
///
/// One slot per track per subscriber: a producer replaces its track's slot and wakes the
/// subscriber, which sends whatever is in the slots and only then takes the next. So a
/// renderer that falls behind sees each track's newest frame, never a queue of stale ones,
/// and at most one frame per track is ever waiting on a socket. (The semantics of a
/// `tokio::sync::watch` per track, in a form a dynamic set of tracks can be awaited as one.)
pub struct FrameFanout {
    subscribers: Mutex<Vec<Arc<FrameSubscriber>>>,
}

struct FrameSubscriber {
    slots: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    notify: tokio::sync::Notify,
}

impl Default for FrameFanout {
    fn default() -> Self {
        Self::new()
    }
}

/// The track key a packed frame carries in its header (`[u32 LE len][UTF-8 key]…`, see
/// `screenshare::codec::pack_frame_bytes`). Lives here, not in the `media`-gated codec, because
/// the fanout is compiled into every client (the TUI builds pollis-core without `media`).
pub(crate) fn frame_track_key(frame: &[u8]) -> Option<&str> {
    let len = u32::from_le_bytes(frame.get(0..4)?.try_into().ok()?) as usize;
    std::str::from_utf8(frame.get(4..4 + len)?).ok()
}

impl FrameFanout {
    pub fn new() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Offer a packed frame (the track key is its header) to every subscriber; a frame
    /// still waiting for the same track is replaced.
    pub fn send(&self, frame: Arc<Vec<u8>>) {
        let Some(track_key) = frame_track_key(&frame) else {
            return;
        };
        let subscribers = self.subscribers.lock().unwrap().clone();
        for sub in subscribers {
            let replaced = sub
                .slots
                .lock()
                .unwrap()
                .insert(track_key.to_string(), Arc::clone(&frame))
                .is_some();
            if replaced {
                FRAMES_DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            sub.notify.notify_one();
        }
    }

    pub fn receiver_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }

    fn subscribe(&self) -> Arc<FrameSubscriber> {
        let sub = Arc::new(FrameSubscriber {
            slots: Mutex::new(HashMap::new()),
            notify: tokio::sync::Notify::new(),
        });
        self.subscribers.lock().unwrap().push(Arc::clone(&sub));
        sub
    }

    fn unsubscribe(&self, sub: &Arc<FrameSubscriber>) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|s| !Arc::ptr_eq(s, sub));
    }
}

impl FrameSubscriber {
    /// Wait for at least one frame, then take every track's newest.
    async fn recv(&self) -> Vec<Arc<Vec<u8>>> {
        loop {
            let taken: Vec<_> = self.slots.lock().unwrap().drain().map(|(_, f)| f).collect();
            if !taken.is_empty() {
                return taken;
            }
            self.notify.notified().await;
        }
    }
}

/// Newtype so `Bytes::from_owner` can borrow an `Arc<Vec<u8>>` without copying.
struct SharedFrame(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedFrame {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Forward frames to one WebSocket client: the newest per track, one send at a time, so a
/// slow renderer is never behind a queue (a frame it cannot take yet is replaced, not kept).
///
/// Zero-copy: every subscriber borrows the one `Arc` frame (#480).
async fn pump_frames(mut socket: WebSocket, fanout: Arc<FrameFanout>) {
    let sub = fanout.subscribe();
    'pump: loop {
        for frame in sub.recv().await {
            let payload = Bytes::from_owner(SharedFrame(frame));
            if socket.send(Message::Binary(payload)).await.is_err() {
                break 'pump;
            }
            FRAMES_SENT.fetch_add(1, Ordering::Relaxed);
        }
    }
    fanout.unsubscribe(&sub);
}

/// Fresh 32-byte hex token, rotated on unlock.
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
        assert_eq!(parse_single_range("bytes=0-10,20-30", 100), Ok(None));
    }

    #[test]
    fn token_constant_time() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    // #480: the payload must borrow the Arc's buffer in place (same address).
    #[test]
    fn shared_frame_send_is_zero_copy() {
        let frame = Arc::new(vec![1u8, 2, 3, 4, 5]);
        let src_ptr = frame.as_ptr();

        let payload = Bytes::from_owner(SharedFrame(Arc::clone(&frame)));

        assert_eq!(payload.as_ref(), &[1, 2, 3, 4, 5]);
        assert_eq!(payload.as_ptr(), src_ptr);
    }

    // The Arc lives exactly as long as some subscriber's `Bytes`.
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
        // Just prove the accessor wires to the two atomics.
        let (sent, dropped) = frame_fanout_counters();
        let _ = (sent, dropped);
    }

    fn packed(track_key: &str, tag: u8) -> Arc<Vec<u8>> {
        let mut v = Vec::new();
        v.extend_from_slice(&(track_key.len() as u32).to_le_bytes());
        v.extend_from_slice(track_key.as_bytes());
        v.extend_from_slice(&[tag; 28]);
        Arc::new(v)
    }

    // Two frames for one track before the subscriber wakes: only the newer is delivered.
    #[tokio::test]
    async fn a_subscriber_that_falls_behind_gets_each_tracks_newest_frame_only() {
        let fanout = FrameFanout::new();
        let sub = fanout.subscribe();
        fanout.send(packed("a", 1));
        fanout.send(packed("a", 2));
        fanout.send(packed("b", 9));
        let mut got = sub.recv().await;
        got.sort_by(|x, y| x.as_slice().cmp(y.as_slice()));
        assert_eq!(got.len(), 2, "one slot per track");
        assert!(got
            .iter()
            .any(|f| f.as_slice() == packed("a", 2).as_slice()));
        assert!(got
            .iter()
            .any(|f| f.as_slice() == packed("b", 9).as_slice()));
        assert!(!got
            .iter()
            .any(|f| f.as_slice() == packed("a", 1).as_slice()));
        // Nothing left: the next recv parks until a new frame arrives.
        let parked = tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await;
        assert!(parked.is_err());
        fanout.send(packed("a", 3));
        assert_eq!(sub.recv().await.len(), 1);
    }

    #[tokio::test]
    async fn unsubscribed_sockets_stop_counting_as_receivers() {
        let fanout = FrameFanout::new();
        assert_eq!(fanout.receiver_count(), 0);
        let sub = fanout.subscribe();
        assert_eq!(fanout.receiver_count(), 1);
        fanout.unsubscribe(&sub);
        assert_eq!(fanout.receiver_count(), 0);
        // A frame with no subscriber is simply dropped on the floor.
        fanout.send(packed("a", 1));
    }

    /// Bring up the real server over a real cached file and return
    /// `(base_url, content_hash)`.
    async fn serving_one_cached_file() -> (String, String) {
        use crate::commands::pin::UnlockState;

        const USER: &str = "media-server-headers";
        let db_key = vec![7u8; 32];
        // 64 hex chars, or `serve_media` rejects it.
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

    /// The webview must not keep a copy: both the 200 and the 206 (video seek) carry `no-store`.
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

        // The 416 goes through the same funnel.
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

    /// Structural half: every body `serve_media` returns comes from `media_response`.
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
