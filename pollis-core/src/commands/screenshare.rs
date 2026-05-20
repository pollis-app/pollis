//! Screen-share over LiveKit.
//!
//! Capture path (Linux AND macOS — one shared subprocess model):
//!   start_screen_share -> bind a Unix socket on a unique tmp path ->
//!   spawn the per-platform capture helper subprocess
//!   (`pollis-capture-linux` / `pollis-capture-macos`) passing the
//!   socket path -> helper drives the OS picker/portal/SCK and writes
//!   BGRx frames over the SHARED `pollis-capture-proto` wire protocol
//!   -> we read the negotiated Format, create a LiveKit
//!   NativeVideoSource + LocalVideoTrack and publish it -> a tokio
//!   reader task pulls frames off the socket, runs libyuv argb_to_i420,
//!   and feeds them into the source.
//!
//! Why a subprocess on BOTH:
//!   - Linux: pulling libpipewire into the same process as libwebrtc +
//!     cpal + webkit2gtk + ashpd reliably crashes inside `pw_init`.
//!   - macOS (issue #283 Phase 2): screencapturekit can throw an
//!     Objective-C `NSUnknownKeyException` on Apple's replayd XPC queue
//!     that Rust `catch_unwind` CANNOT catch — it reaches
//!     `std::terminate` and aborts the whole app. Isolating SCK in a
//!     helper means the terminate kills only the helper; the parent
//!     observes the socket close and surfaces a structured error.
//!   Isolating capture in its own process makes the linkage soup / the
//!   uncatchable-throw the kernel's problem on both platforms.
//!
//! Windows still captures in-process via `windows-capture` — WGC is a
//! clean in-proc linkage with no analogous uncatchable-exception hazard.
//!
//! Render path (any OS, triggered when a remote participant publishes a
//! screenshare track in the joined voice room):
//!   voice.rs room loop sees RemoteTrack::Video ->
//!   on_remote_video_subscribed spawns a drain task ->
//!   NativeVideoStream yields frames -> to_i420 -> header + Y/U/V
//!   planes packed as raw bytes -> Tauri Channel as
//!   InvokeResponseBody::Raw -> ArrayBuffer in webview -> WebGL canvas.
//!
//! Two Channels: `events` for low-volume JSON lifecycle, `frames` for
//! raw binary plane data.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use libwebrtc::{
    prelude::{RtcVideoSource, VideoFrame, VideoRotation},
    video_frame::{I420Buffer, VideoBuffer},
    video_source::{native::NativeVideoSource, VideoResolution},
    video_stream::native::NativeVideoStream,
};
use livekit::{
    options::{TrackPublishOptions, VideoCodec},
    prelude::*,
    track::{LocalTrack, LocalVideoTrack, RemoteVideoTrack},
};

use crate::{error::Result, sink::EventSink, state::AppState};

// ── Events to the frontend ────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenShareEvent {
    LocalStarted { width: u32, height: u32 },
    LocalStopped,
    /// Capture helper exited / errored before publishing.
    LocalError { message: String },
    /// The platform genuinely cannot screen-share (distinct from a
    /// permission denial the user can fix). Today this is the Linux
    /// "Wayland session with no xdg-desktop-portal ScreenCast backend"
    /// case (Cinnamon/MATE/XFCE-on-Wayland). The frontend shows an
    /// "unsupported desktop" message, NOT a "grant permission" prompt.
    LocalUnsupported { message: String },
    RemoteStarted {
        track_key: String,
        identity: String,
        width: u32,
        height: u32,
    },
    RemoteStopped { track_key: String },
}

// ── Resolution / FPS caps ─────────────────────────────────────────────────
//
// A 4K or ultrawide source must never reach the software VP8 encoder at
// native resolution — it pegs a core and tanks the call. Cap every capture
// path (macOS SCK, Windows WGC, Linux pipewire reader) to 1080p / 60fps
// before publishing, preserving aspect ratio with even dims (VP8 + I420
// 4:2:0 chroma require even width/height).

const MAX_SHARE_WIDTH: u32 = 1920;
const MAX_SHARE_HEIGHT: u32 = 1080;
const MAX_SHARE_FPS: u32 = 60;

/// Minimum spacing between published frames to enforce `MAX_SHARE_FPS`.
/// Used by the Linux reader (the macOS/Windows native pipelines are
/// already display-rate-locked and SCK/WGC honour their own interval
/// settings; an extra clamp there would only add a timestamp compare with
/// no benefit, so the FPS clamp lives only where frames can outrun the
/// cap — the Linux pipewire path).
const MIN_FRAME_INTERVAL: std::time::Duration =
    std::time::Duration::from_nanos(1_000_000_000 / MAX_SHARE_FPS as u64);

/// If a source exceeds the cap, return the largest even-dim'd size that
/// fits inside `MAX_SHARE_WIDTH`×`MAX_SHARE_HEIGHT` while preserving aspect
/// ratio. Returns `None` when the source already fits (no scale needed —
/// keeps the fast path allocation-free). Inputs are assumed already
/// even-floored by the caller.
fn capped_dims(width: u32, height: u32) -> Option<(u32, u32)> {
    if width <= MAX_SHARE_WIDTH && height <= MAX_SHARE_HEIGHT {
        return None;
    }
    if width == 0 || height == 0 {
        return None;
    }
    // Scale by the tighter of the two axis ratios so both fit. f64 keeps
    // precision for ultrawide ratios; this runs once per resolution
    // change at most when wired through the announce path, and at worst
    // once per frame as a couple of cmp+mul — negligible vs. the encode.
    let sw = MAX_SHARE_WIDTH as f64 / width as f64;
    let sh = MAX_SHARE_HEIGHT as f64 / height as f64;
    let scale = sw.min(sh);
    let mut cw = (width as f64 * scale).round() as u32;
    let mut ch = (height as f64 * scale).round() as u32;
    // Even dims for I420 chroma; clamp so rounding can't exceed the cap.
    cw = (cw.min(MAX_SHARE_WIDTH)) & !1;
    ch = (ch.min(MAX_SHARE_HEIGHT)) & !1;
    if cw == 0 || ch == 0 {
        return None;
    }
    Some((cw, ch))
}

/// Apply `argb_to_i420` then, if the source exceeds the cap, downscale via
/// `I420Buffer::scale` (libyuv `I420Scale` under the hood). The full-res
/// I420Buffer is unavoidable — `argb_to_i420` needs an I420 destination
/// and libwebrtc 0.3.29 exposes neither an ARGB-scale nor an `i420_scale`
/// free function — so we convert at native res then let libyuv produce the
/// scaled buffer. That's exactly one extra buffer alloc on the over-cap
/// path and zero on the (common) within-cap path; last-frame-wins
/// backpressure upstream is unchanged. Shared by all three OS push paths.
fn convert_and_cap(
    width: i32,
    height: i32,
    src_stride: u32,
    argb: &[u8],
) -> I420Buffer {
    let mut buffer = I420Buffer::new(width as u32, height as u32);
    {
        let (sy, su, sv) = buffer.strides();
        let (dy, du, dv) = buffer.data_mut();
        libwebrtc::native::yuv_helper::argb_to_i420(
            argb, src_stride, dy, sy, du, su, dv, sv, width, height,
        );
    }
    match capped_dims(width as u32, height as u32) {
        Some((cw, ch)) => buffer.scale(cw as i32, ch as i32),
        None => buffer,
    }
}

// There is no "stalled" / "paused" concept anywhere in screenshare:
// when capture is idle (static screen on Wayland, etc.) we simply
// stop pushing frames. The viewer's canvas keeps showing the last
// painted frame and the streamer's UI keeps showing "LIVE" — both
// indistinguishable from a stream of unchanging frames. A previous
// implementation had a 2-second watchdog emitting LocalStalled /
// RemoteStalled events with a "Stream paused" overlay, which
// misrepresented normal idle behaviour as a failure. Removed.

/// Track key the local outgoing capture is mirrored under so the sharer can
/// watch a low-rate preview of their own stream. Reserved sentinel — never
/// collides with a remote "{identity}-{sid}" key.
pub const LOCAL_PREVIEW_KEY: &str = "__local_preview__";

/// The self-preview answers "is my stream actually going out?", not
/// fidelity. Cap it well below capture rate so the extra I420 pack + IPC
/// stays off the hot path.
const PREVIEW_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Surface a genuine capture/permission/portal/WGC/SCK failure: keep the
/// raw cause on stderr, emit a `LocalError { message }` so the frontend
/// reacts even when the failure happens after `start_screen_share` already
/// returned (or when the caller swallows the Result), and return a
/// structured human-readable error. Used by every start failure branch.
/// Plain user cancellation does NOT go through here — that's a normal flow,
/// not an error the UI must react to.
async fn fail_capture(state: &Arc<AppState>, human: String) -> crate::error::Error {
    eprintln!("[screenshare] capture failed: {human}");
    let ev = {
        let ss = state.screenshare.lock().await;
        ss.events.clone()
    };
    if let Some(ev) = ev {
        let _ = ev.send(ScreenShareEvent::LocalError {
            message: human.clone(),
        });
    }
    crate::error::Error::Other(anyhow::anyhow!(human))
}

// ── Top-level state ───────────────────────────────────────────────────────

/// A connected capture-helper session — the spawned child plus the two
/// halves of its Unix-socket connection. We split the stream because the
/// parent both **writes** (Select message on macOS) and **reads** (Format
/// + Frames). Owning both halves lets us park the writer in state while
/// the reader task drains frames; dropping the writer would signal EOF
/// to the helper's reader and risk an early exit.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct HelperSession {
    pub child: tokio::process::Child,
    pub reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    pub writer: tokio::net::unix::OwnedWriteHalf,
}

pub struct ScreenShareState {
    pub events: Option<Arc<dyn EventSink<ScreenShareEvent>>>,
    pub frames: Option<Arc<dyn RawSink>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,
    /// macOS picker phase: helper is spawned and has sent its `Sources`
    /// list; we're waiting on the user's pick from the in-app picker.
    /// `start_screen_share` consumes this (sends `Select`, transitions
    /// to capture); `cancel_screen_share_picker` discards it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub picker_session: Option<HelperSession>,
    /// Linux/macOS: handle to the capture helper subprocess.
    /// Linux for libpipewire isolation; macOS for SCK uncatchable-ObjC
    /// isolation (#283 Phase 2). Dropping/killing it terminates capture;
    /// the reader task observes the socket close.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_helper: Option<tokio::process::Child>,
    /// Kept open for the lifetime of the capture so the helper's read
    /// side doesn't see EOF and exit early. Dropped on stop.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    /// Linux/macOS: the supervising task that reads from the helper's
    /// socket and pushes frames into the LiveKit source.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Windows: the dedicated thread running the blocking WGC capture
    /// (`GraphicsCaptureApiHandler::start`). We can't use
    /// `start_free_threaded` here: it requires the capture item be `Send`,
    /// but `windows-capture`'s picker hands back a `PickedGraphicsCaptureItem`
    /// that owns an `HwndGuard` (`!Send`). So the picker + capture run on
    /// their own thread and `stop_screen_share` ends them by flipping
    /// `windows_active` (the frame callback then calls
    /// `InternalCaptureControl::stop`). The handle is detached, not
    /// force-joined — the fence guarantees no post-stop source access.
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: per-session fence, same role as `macos_active`. The WGC
    /// frame callback checks it before touching the LiveKit source; a
    /// fresh Arc per session so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

    /// Per-remote-track drain task. Key = "{identity}-{sid}".
    pub remote_drain_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
}

impl ScreenShareState {
    pub fn new() -> Self {
        Self {
            events: None,
            frames: None,
            local_track: None,
            local_source: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            picker_session: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            local_helper: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            local_writer: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            local_reader_task: None,
            #[cfg(target_os = "windows")]
            windows_thread: None,
            #[cfg(target_os = "windows")]
            windows_active: None,
            remote_drain_tasks: std::collections::HashMap::new(),
        }
    }
}

/// Re-export of the binary-bytes sink (now defined in `crate::sink` as a
/// neutral home so the terminal path can share it). Kept here so existing
/// `commands::screenshare::RawSink` imports keep working.
pub use crate::sink::RawSink;

// ── Tauri-facing commands ─────────────────────────────────────────────────

pub async fn subscribe_screen_share_events(
    sink: Arc<dyn EventSink<ScreenShareEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut ss = state.screenshare.lock().await;
    ss.events = Some(sink);
    Ok(())
}

pub async fn subscribe_screen_share_frames(
    sink: Arc<dyn RawSink>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut ss = state.screenshare.lock().await;
    ss.frames = Some(sink);
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub async fn start_screen_share(
    _state: &Arc<AppState>,
    _selection: Option<pollis_capture_proto::Selection>,
) -> Result<()> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "screen share is not implemented on this OS yet"
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub async fn enumerate_screen_sources(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub async fn cancel_screen_share_picker(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}

// ── Shared helper-subprocess capture path (Linux + macOS) ─────────────────
//
// One implementation drives both per-platform helpers. The only
// per-OS difference is which helper binary is spawned
// (`capture_helper_name()`); everything after — socket accept, the
// `pollis-capture-proto` Format/Frame/Error decode, LiveKit publish,
// FPS cap, libyuv ARGB->I420 — is identical. This is exactly the
// de-risking #283 Phase 2 buys: every SCK call now runs in a process
// whose death the parent already tolerates.

/// Spawn the per-platform capture helper and wait for it to connect back
/// over a fresh Unix socket. Returns the established session split into
/// read/write halves so the parent can both send `Select` (macOS picker
/// reply) and read `Format`/`Frame` messages. Used by both
/// `enumerate_screen_sources` (macOS picker phase) and
/// `start_screen_share` (Linux/Windows direct path).
#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn spawn_and_accept_helper(
    state: &Arc<AppState>,
) -> Result<HelperSession> {
    use tokio::net::UnixListener;

    let socket_path = std::env::temp_dir().join(format!(
        "pollis-capture-{}-{}.sock",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[screenshare] bind unix socket: {e}");
            return Err(fail_capture(
                state,
                "Could not set up the screen-capture channel. Please try again.".into(),
            )
            .await);
        }
    };

    let helper_path = match locate_capture_helper() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[screenshare] locate helper: {e}");
            return Err(fail_capture(
                state,
                "Screen-capture helper not found. Reinstall Pollis or rebuild the capture helper.".into(),
            )
            .await);
        }
    };
    eprintln!(
        "[screenshare] spawning helper {} on socket {}",
        helper_path.display(),
        socket_path.display()
    );
    let helper = tokio::process::Command::new(&helper_path)
        .arg("--socket")
        .arg(&socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn();
    let mut helper = match helper {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[screenshare] spawn {}: {e}", helper_path.display());
            return Err(fail_capture(
                state,
                "Could not launch the screen-capture helper. Please try again.".into(),
            )
            .await);
        }
    };

    let accept_fut = listener.accept();
    let (stream, _addr) = tokio::select! {
        res = accept_fut => match res {
            Ok(r) => {
                eprintln!("[screenshare] helper connected");
                r
            }
            Err(e) => {
                eprintln!("[screenshare] accept: {e}");
                let _ = std::fs::remove_file(&socket_path);
                return Err(fail_capture(
                    state,
                    "Screen-capture helper failed to connect. Please try again.".into(),
                )
                .await);
            }
        },
        status = helper.wait() => {
            eprintln!("[screenshare] helper exited before connecting: {status:?}");
            let _ = std::fs::remove_file(&socket_path);
            return Err(fail_capture(
                state,
                "Screen capture could not start (helper exited). Check screen-capture permission and try again.".into(),
            )
            .await);
        }
    };
    let _ = std::fs::remove_file(&socket_path);

    let (read_half, write_half) = stream.into_split();
    Ok(HelperSession {
        child: helper,
        reader: tokio::io::BufReader::with_capacity(64 * 1024, read_half),
        writer: write_half,
    })
}

/// macOS: spawn the helper, let it enumerate via `SCShareableContent`,
/// and return the list of capturable displays + windows. The helper
/// stays parked in `state.screenshare.picker_session` waiting for the
/// upcoming `start_screen_share(Some(selection))`. On Linux/Windows this
/// returns `Err`; the frontend should never call it on those platforms
/// (the portal/WGC picker handles selection).
#[cfg(target_os = "macos")]
pub async fn enumerate_screen_sources(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    use pollis_capture_proto::{read_msg, CaptureMsg};

    // Discard any previous picker session that never got chosen.
    cancel_screen_share_picker(state).await.ok();

    let mut session = spawn_and_accept_helper(state).await?;
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        read_msg(&mut session.reader),
    )
    .await
    .map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!(
            "screen-capture helper did not return a source list (timed out)"
        ))
    })?;
    let list = match msg {
        Ok(Some(CaptureMsg::Sources(list))) => list,
        Ok(Some(CaptureMsg::Error { message })) => {
            // Pass the helper's message through verbatim — the
            // frontend's `friendlyScreenShareError` collapses it into
            // a short, user-facing sentence (TCC denial, no displays,
            // etc). Wrapping here would just lengthen the string the
            // status bar has to render.
            return Err(crate::error::Error::Other(anyhow::anyhow!(message)));
        }
        Ok(Some(other)) => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "unexpected helper message during enumeration: {other:?}"
            )));
        }
        Ok(None) => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "screen-capture helper exited before sending source list"
            )));
        }
        Err(e) => return Err(crate::error::Error::Other(anyhow::anyhow!(e))),
    };

    let mut ss = state.screenshare.lock().await;
    ss.picker_session = Some(session);
    Ok(list)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub async fn enumerate_screen_sources(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    // The system portal (Linux) or system picker (Windows) handles
    // source selection. The frontend should not call this on these
    // platforms — but returning an empty list is a safer no-op than
    // an error if it ever does.
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

/// Discard a parked picker session — used when the user backs out of
/// the in-app picker without selecting a source.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub async fn cancel_screen_share_picker(state: &Arc<AppState>) -> Result<()> {
    let session = {
        let mut ss = state.screenshare.lock().await;
        ss.picker_session.take()
    };
    if let Some(mut session) = session {
        let _ = session.child.kill().await;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub async fn cancel_screen_share_picker(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub async fn start_screen_share(
    state: &Arc<AppState>,
    selection: Option<pollis_capture_proto::Selection>,
) -> Result<()> {
    use pollis_capture_proto::encode_select;
    use tokio::io::AsyncWriteExt;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Re-share from a clean slate: kill any lingering helper/reader from a
    // previous session before spawning a new one.
    {
        let has_prev = {
            let ss = state.screenshare.lock().await;
            ss.local_track.is_some()
                || ss.local_helper.is_some()
                || ss.local_reader_task.is_some()
        };
        if has_prev {
            let _ = stop_screen_share(state).await;
        }
    }

    // Acquire the helper: reuse a parked picker session (macOS, after
    // enumerate→user-pick) or spawn a fresh one (Linux portal path).
    let parked = {
        let mut ss = state.screenshare.lock().await;
        ss.picker_session.take()
    };
    let mut session = match parked {
        Some(s) => s,
        None => spawn_and_accept_helper(state).await?,
    };

    // macOS picker reply. The helper is parked between Sources and
    // Format; Select unblocks it. Linux helpers ignore this (no such
    // protocol message is sent without a Selection).
    if let Some(sel) = &selection {
        if let Err(e) = session.writer.write_all(&encode_select(sel)).await {
            eprintln!("[screenshare] send Select: {e}");
            return Err(fail_capture(
                state,
                "Could not deliver the screen-share selection to the helper. Please try again.".into(),
            )
            .await);
        }
        let _ = session.writer.flush().await;
    }

    // Park the helper handle + writer in state so a concurrent
    // `stop_screen_share` (e.g. user clicked stop while waiting for
    // the picker, or the publish below errors and they cancel) can
    // kill it via the standard cleanup path. The writer stays open
    // for the capture's lifetime so the helper's read side doesn't
    // see EOF and exit early.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_helper = Some(session.child);
        ss.local_writer = Some(session.writer);
    }

    // Read the first protocol message; expect Format. Anything
    // else (or EOF) is a hard failure. Generous 5-min timeout
    // covers the user staring at the portal picker on Linux.
    let mut reader = session.reader;
    eprintln!("[screenshare] awaiting video format from helper");
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        pollis_capture_proto::read_msg(&mut reader),
    )
    .await;
    let msg = match read_result {
        Ok(r) => r,
        Err(_) => {
            // 300s elapsed with no format. The user staring at the OS
            // picker is the expected long case; hitting this means the
            // portal never produced a stream.
            stop_screen_share(state).await.ok();
            return Err(fail_capture(
                state,
                "Screen capture timed out waiting for a source. Please try again.".into(),
            )
            .await);
        }
    };
    use pollis_capture_proto::CaptureMsg;
    let (width, height) = match msg {
        Ok(Some(CaptureMsg::Format { width, height })) => {
            eprintln!("[screenshare] helper announced {}x{}", width, height);
            (width & !1, height & !1)
        }
        Ok(Some(CaptureMsg::Frame { .. }))
        | Ok(Some(CaptureMsg::Sources(_)))
        | Ok(Some(CaptureMsg::Select(_))) => {
            eprintln!("[screenshare] helper sent unexpected message before format");
            stop_screen_share(state).await.ok();
            return Err(fail_capture(
                state,
                "Screen capture failed (protocol error). Please try again.".into(),
            )
            .await);
        }
        Ok(Some(CaptureMsg::Error { message })) => {
            // The helper relays the failure cause as a prefixed string.
            // Split the three distinct shapes the old code collapsed
            // into one "permission" message:
            //   - `unsupported:` — the desktop environment has no
            //     ScreenCast backend at all (Linux Cinnamon/MATE/XFCE
            //     on Wayland). NOT something the user can grant; emit
            //     LocalUnsupported so the UI shows a different message.
            //   - `cancel` / `dismiss` — normal user cancellation, not
            //     an error to surface as LocalError.
            //   - everything else (portal errors, denied permission,
            //     SCK failures) — a genuine capture failure.
            eprintln!("[screenshare] helper error: {message}");
            stop_screen_share(state).await.ok();
            let lower = message.to_lowercase();
            if lower.starts_with("unsupported:") || lower.contains("no screencast") {
                let human = "Screen sharing isn't available on this desktop. \
                    Your desktop environment doesn't provide a screen-sharing \
                    backend (xdg-desktop-portal ScreenCast). GNOME, KDE or an \
                    X11 session support it."
                    .to_string();
                let ev = {
                    let ss = state.screenshare.lock().await;
                    ss.events.clone()
                };
                if let Some(ev) = ev {
                    let _ = ev.send(ScreenShareEvent::LocalUnsupported {
                        message: human.clone(),
                    });
                }
                return Err(crate::error::Error::Other(anyhow::anyhow!(human)));
            }
            // User-cancelled the source picker. The helper prefixes these
            // with `cancel:`. Treat as a no-op: clean up (already done
            // above), exit silently with Ok so the frontend's
            // start().catch() never fires and no toast appears. No
            // LocalStarted event was emitted, so the store stays at
            // screenShareLocalActive=false.
            if lower.starts_with("cancel:")
                || lower.starts_with("cancelled")
                || lower.contains("dismiss")
            {
                return Ok(());
            }
            return Err(fail_capture(
                state,
                "Screen capture could not start. Check screen-capture permission and try again.".into(),
            )
            .await);
        }
        Ok(None) => {
            eprintln!("[screenshare] helper closed socket before format");
            stop_screen_share(state).await.ok();
            return Err(fail_capture(
                state,
                "Screen capture ended before it started. Please try again.".into(),
            )
            .await);
        }
        Err(e) => {
            eprintln!("[screenshare] helper read error: {e}");
            stop_screen_share(state).await.ok();
            return Err(fail_capture(
                state,
                "Lost the screen-capture connection. Please try again.".into(),
            )
            .await);
        }
    };

    if width == 0 || height == 0 {
        eprintln!("[screenshare] helper announced zero-size format");
        stop_screen_share(state).await.ok();
        return Err(fail_capture(
            state,
            "Screen capture returned an invalid source size. Please try again.".into(),
        )
        .await);
    }

    // 5. Create the LiveKit track + publish.
    let source = NativeVideoSource::new(
        VideoResolution { width, height },
        true, /* is_screencast */
    );
    let track = LocalVideoTrack::create_video_track(
        "screenshare",
        RtcVideoSource::Native(source.clone()),
    );
    eprintln!("[screenshare] publishing track {}x{}", width, height);
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: VideoCodec::VP8,
                ..Default::default()
            },
        )
        .await
    {
        eprintln!("[screenshare] publish error: {e}");
        stop_screen_share(state).await.ok();
        return Err(fail_capture(
            state,
            "Could not publish the screen-share to the call. Check your connection and try again.".into(),
        )
        .await);
    }
    eprintln!("[screenshare] track published");

    // 6. Spawn the supervising reader task. It owns the socket + the
    //    LiveKit source from here on; on EOF / error it just exits and
    //    relies on stop_screen_share for the rest of cleanup.
    let source_for_task = source.clone();
    let (events_for_task, frames_for_task) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };
    let reader_task = tokio::spawn(async move {
        let mut last_preview: Option<std::time::Instant> = None;
        // FPS cap lives here — the lowest-overhead point on the Linux
        // path. pipewire can deliver at the source's native refresh
        // (144Hz+ displays); dropping frames before the libyuv convert +
        // VP8 encode keeps the SW encoder off a treadmill. macOS SCK /
        // Windows WGC honour their own MinimumUpdateInterval so they
        // don't need this extra clamp.
        let mut last_pushed: Option<std::time::Instant> = None;
        // No keep-alive / no synthetic frame replay: when the captured
        // surface is idle, pipewire goes silent and we just stop
        // pushing. The viewer's canvas retains the last painted frame
        // (the GL context isn't torn down), so the share looks
        // identical to one where the content happens to be unchanging.
        // We trade nothing on the user-visible side and save the
        // bandwidth + CPU of re-encoding identical pixels.
        loop {
            match pollis_capture_proto::read_msg(&mut reader).await {
                Ok(Some(CaptureMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                })) => {
                    // FPS cap: skip frames arriving faster than
                    // MAX_SHARE_FPS. Cheap Instant compare, last-frame-
                    // wins is preserved (we just drop the early one).
                    if let Some(t) = last_pushed {
                        if t.elapsed() < MIN_FRAME_INTERVAL {
                            continue;
                        }
                    }
                    last_pushed = Some(std::time::Instant::now());
                    let preview = match &frames_for_task {
                        Some(sink)
                            if last_preview
                                .map_or(true, |t| t.elapsed() >= PREVIEW_MIN_INTERVAL) =>
                        {
                            last_preview = Some(std::time::Instant::now());
                            Some(sink.as_ref())
                        }
                        _ => None,
                    };
                    push_frame(
                        &source_for_task,
                        width,
                        height,
                        stride,
                        timestamp_us,
                        &bgrx,
                        preview,
                    );
                }
                Ok(Some(CaptureMsg::Format { .. })) => {
                    // Renegotiation mid-stream — currently unsupported,
                    // but harmless to ignore. The next frame will use
                    // the new dimensions; LiveKit's NativeVideoSource
                    // tolerates per-frame size changes.
                }
                Ok(Some(CaptureMsg::Sources(_))) | Ok(Some(CaptureMsg::Select(_))) => {
                    // Only valid during the picker handshake — should
                    // never appear once frames are flowing. Ignore.
                }
                Ok(Some(CaptureMsg::Error { message })) => {
                    if let Some(ev) = &events_for_task {
                        let _ = ev.send(ScreenShareEvent::LocalError { message });
                    }
                    break;
                }
                Ok(None) => break,
                Err(e) => {
                    if let Some(ev) = &events_for_task {
                        let _ = ev.send(ScreenShareEvent::LocalError {
                            message: format!("read: {e}"),
                        });
                    }
                    break;
                }
            }
        }
    });

    // 7. Save state and announce. Helper handle was parked earlier.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.local_reader_task = Some(reader_task);
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    Ok(())
}

// ── Windows path ──────────────────────────────────────────────────────────
//
// In-process via the `windows-capture` crate (Windows.Graphics.Capture).
// Like macOS, no subprocess is needed — WGC is a clean in-proc linkage and
// doesn't fight libwebrtc/cpal/Tauri the way Linux's libpipewire does.
//
// Capture flow (mirrors macOS):
//   1. Show the system GraphicsCapturePicker (display/window/app).
//   2. Create the LiveKit NativeVideoSource + LocalVideoTrack, publish to
//      the current voice room as Screenshare/VP8.
//   3. start_free_threaded a handler that owns a clone of the source and
//      converts every BGRA8 WGC frame to I420 inline (off the tokio
//      runtime — WGC pumps on its own worker thread).
//   4. Stash the CaptureControl in state so stop is synchronous + ordered
//      with the track unpublish.
//
// The picker + session start run inside one spawn_blocking: the picker
// pumps a message loop and the picked item is not Send, so it can't cross
// the await boundary. We publish first (provisional resolution; WGC's
// real per-frame dimensions drive the stream and LiveKit tolerates a
// per-frame size change) so no initial frames are lost.
#[cfg(target_os = "windows")]
pub async fn start_screen_share(
    state: &Arc<AppState>,
    _selection: Option<pollis_capture_proto::Selection>,
) -> Result<()> {
    use std::sync::atomic::AtomicBool;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Re-share must start from a clean slate (same rationale as macOS).
    {
        let has_prev = {
            let ss = state.screenshare.lock().await;
            ss.local_track.is_some() || ss.windows_thread.is_some()
        };
        if has_prev {
            let _ = stop_screen_share(state).await;
        }
    }

    // 1. LiveKit source + track. Provisional resolution; the first WGC
    //    frame carries the true selection size and LiveKit's
    //    NativeVideoSource tolerates per-frame size changes.
    let source = NativeVideoSource::new(
        VideoResolution {
            width: 1920,
            height: 1080,
        },
        true, /* is_screencast */
    );
    let track = LocalVideoTrack::create_video_track(
        "screenshare",
        RtcVideoSource::Native(source.clone()),
    );
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: VideoCodec::VP8,
                ..Default::default()
            },
        )
        .await
    {
        eprintln!("[screenshare] publish error: {e}");
        return Err(fail_capture(
            state,
            "Could not publish the screen-share to the call. Check your connection and try again.".into(),
        )
        .await);
    }
    eprintln!("[screenshare] track published");

    // 2. Fresh per-session fence + the frames sink for the self-preview.
    let active_flag = std::sync::Arc::new(AtomicBool::new(true));
    let (frames_sink, events_sink) = {
        let ss = state.screenshare.lock().await;
        (ss.frames.clone(), ss.events.clone())
    };

    // 3. Picker + blocking capture on a dedicated owned thread.
    //
    // `windows-capture`'s `GraphicsCapturePicker` hands back a
    // `PickedGraphicsCaptureItem` that owns an `HwndGuard` (`!Send`), so
    // it cannot go through `start_free_threaded` (which requires the item
    // be `Send`). Instead run the picker, build `Settings`, and call the
    // blocking `start()` all on one thread we own — nothing `!Send` ever
    // crosses a thread boundary or an `.await`. The thread reports the
    // picked size (or a picker cancel/error) back over a oneshot; then
    // `start()` blocks the thread, pumping WGC, until the frame callback
    // observes `active == false` and calls `InternalCaptureControl::stop`.
    let flags = WindowsCaptureFlags {
        source: source.clone(),
        active: std::sync::Arc::clone(&active_flag),
        frames: frames_sink,
    };
    // Outcome the dedicated WGC thread reports before it blocks in
    // start(): the negotiated size, a clean user cancel (not surfaced as
    // an error the UI must react to), or a genuine capture failure
    // (surfaced via LocalError).
    enum WgcStart {
        Size(u32, u32),
        Cancelled,
        Failed(String),
    }
    let (size_tx, size_rx) = tokio::sync::oneshot::channel::<WgcStart>();
    // The thread blocks in start() long after this command returns; a
    // start() error there is a genuine post-return capture failure, so
    // give the thread an events handle to emit LocalError itself.
    let events_for_thread = events_sink.clone();
    let capture_thread = std::thread::Builder::new()
        .name("wgc-screenshare".into())
        .spawn(move || {
            use windows_capture::capture::GraphicsCaptureApiHandler;
            use windows_capture::graphics_capture_picker::GraphicsCapturePicker;
            use windows_capture::settings::{
                ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
                MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
            };

            let picked = match GraphicsCapturePicker::pick_item() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let _ = size_tx.send(WgcStart::Cancelled);
                    return;
                }
                Err(e) => {
                    eprintln!("[screenshare] WGC picker error: {e}");
                    let _ = size_tx.send(WgcStart::Failed(
                        "Windows could not open the screen-share picker. Please try again."
                            .into(),
                    ));
                    return;
                }
            };
            let (sw, sh) = match picked.size() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[screenshare] WGC picker size: {e}");
                    let _ = size_tx.send(WgcStart::Failed(
                        "Could not read the selected screen-share source. Please try again."
                            .into(),
                    ));
                    return;
                }
            };
            // Force even dims for VP8 + I420 chroma alignment.
            let width = (sw.max(0) as u32) & !1;
            let height = (sh.max(0) as u32) & !1;
            if width == 0 || height == 0 {
                eprintln!("[screenshare] WGC picker reported zero-size selection");
                let _ = size_tx.send(WgcStart::Failed(
                    "The selected screen-share source has an invalid size. Please try again."
                        .into(),
                ));
                return;
            }
            eprintln!("[screenshare] windows picked {}x{}", width, height);

            // Bgra8 so the bytes are B,G,R,A in memory — identical to the
            // macOS/Linux paths feeding libwebrtc argb_to_i420, no swizzle.
            let settings = Settings::new(
                picked,
                CursorCaptureSettings::WithCursor,
                DrawBorderSettings::Default,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                flags,
            );

            // Capture is about to start; hand the size back so the caller
            // can announce and unblock.
            let _ = size_tx.send(WgcStart::Size(width, height));

            // Blocks here, pumping WGC, until the frame callback sees the
            // fence flipped and stops the session. An error before the
            // fence is flipped is a genuine capture failure that happens
            // after this command already returned Ok — surface it via
            // LocalError so the frontend reacts.
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                eprintln!("[screenshare] WGC start/stop: {e}");
                if let Some(ev) = &events_for_thread {
                    let _ = ev.send(ScreenShareEvent::LocalError {
                        message:
                            "Screen capture stopped unexpectedly. Please try sharing again."
                                .into(),
                    });
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("spawn wgc capture thread: {e}"))?;

    let (width, height) = match size_rx.await {
        Ok(WgcStart::Size(w, h)) => (w, h),
        Ok(WgcStart::Cancelled) => {
            // Normal user cancel — roll back the publish, return without
            // emitting LocalError (not a failure the UI must react to).
            let sid = track.sid();
            let _ = room.local_participant().unpublish_track(&sid).await;
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "screen share cancelled"
            )));
        }
        Ok(WgcStart::Failed(msg)) => {
            let sid = track.sid();
            let _ = room.local_participant().unpublish_track(&sid).await;
            return Err(fail_capture(state, msg).await);
        }
        Err(_) => {
            let sid = track.sid();
            let _ = room.local_participant().unpublish_track(&sid).await;
            return Err(fail_capture(
                state,
                "Screen capture failed to start. Please try again.".into(),
            )
            .await);
        }
    };

    // 4. Stash for stop_screen_share + announce.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.windows_thread = Some(capture_thread);
        ss.windows_active = Some(std::sync::Arc::clone(&active_flag));
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    let _ = events_sink;
    Ok(())
}

#[cfg(target_os = "windows")]
struct WindowsCaptureFlags {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
}

#[cfg(target_os = "windows")]
struct WindowsCaptureHandler {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
    // on_frame_arrived takes &mut self (WGC serializes the callback), so a
    // plain field suffices — no Mutex unlike the macOS &self handler.
    last_preview: Option<std::time::Instant>,
}

#[cfg(target_os = "windows")]
impl windows_capture::capture::GraphicsCaptureApiHandler for WindowsCaptureHandler {
    type Flags = WindowsCaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(
        ctx: windows_capture::capture::Context<Self::Flags>,
    ) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            source: ctx.flags.source,
            active: ctx.flags.active,
            frames: ctx.flags.frames,
            last_preview: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut windows_capture::frame::Frame<'_>,
        capture_control: windows_capture::graphics_capture_api::InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        // Stop fence: a teardown flips this; end the pump from inside.
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            capture_control.stop();
            return Ok(());
        }
        let mut buffer = frame.buffer().map_err(|e| -> Self::Error { Box::new(e) })?;
        let width = buffer.width();
        let height = buffer.height();
        // row_pitch is the GPU-aligned stride (>= width*4); argb_to_i420
        // consumes it directly, same as the macOS bytes_per_row path.
        let stride = buffer.row_pitch();
        let bgra = buffer.as_raw_buffer();
        let timestamp_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        let preview = match &self.frames {
            Some(sink)
                if self
                    .last_preview
                    .map_or(true, |t| t.elapsed() >= PREVIEW_MIN_INTERVAL) =>
            {
                self.last_preview = Some(std::time::Instant::now());
                Some(sink.as_ref())
            }
            _ => None,
        };
        push_frame_windows(&self.source, width, height, stride, timestamp_us, bgra, preview);
        Ok(())
    }

    fn on_closed(&mut self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn push_frame_windows(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgra: &[u8],
    preview: Option<&dyn RawSink>,
) {
    // VP8 + I420 require even dimensions.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // Convert (WGC Bgra8 == little-endian ARGB) and cap to 1080p.
    let buffer = convert_and_cap(w, h, stride, bgra);
    let (out_w, out_h) = (buffer.width(), buffer.height());
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    source.capture_frame(&frame);
    if let Some(sink) = preview {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            out_w,
            out_h,
            timestamp_us,
            &frame.buffer,
        );
        let _ = sink.send(bytes);
    }
}

pub async fn stop_screen_share(state: &Arc<AppState>) -> Result<()> {
    let room;
    let track;
    let source_to_drop;
    let ev_opt;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut helper;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let reader;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut picker;
    #[cfg(target_os = "windows")]
    let windows_thread;
    #[cfg(target_os = "windows")]
    let windows_active;
    {
        let mut ss = state.screenshare.lock().await;
        track = ss.local_track.take();
        // Keep the source alive locally until after the SCK stream is fully
        // torn down + the track is unpublished. Releasing it from state now
        // would otherwise let the next reference drop free its backing
        // while in-flight handler calls are still firing.
        source_to_drop = ss.local_source.take();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            helper = ss.local_helper.take();
            reader = ss.local_reader_task.take();
            picker = ss.picker_session.take();
            // Dropping the writer closes our half of the socket so the
            // helper sees EOF and exits cleanly even if its parent-death
            // poll is mid-sleep.
            ss.local_writer = None;
        }
        #[cfg(target_os = "windows")]
        {
            windows_thread = ss.windows_thread.take();
            windows_active = ss.windows_active.take();
        }
        ev_opt = ss.events.clone();
        let voice = state.voice.lock().await;
        room = voice.room.clone();
    }

    // Nothing was live (e.g. the defensive pre-share teardown, or an
    // on_room_disconnected with no active share). Return without firing a
    // spurious LocalStopped — that would flip the UI's share state off
    // right as a fresh share is starting.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let had_session = track.is_some()
        || source_to_drop.is_some()
        || helper.is_some()
        || reader.is_some()
        || picker.is_some();
    #[cfg(target_os = "windows")]
    let had_session =
        track.is_some() || source_to_drop.is_some() || windows_thread.is_some();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let had_session = track.is_some() || source_to_drop.is_some();
    if !had_session {
        return Ok(());
    }

    // Linux + macOS: identical teardown. Abort the reader task, then
    // kill the helper subprocess. On macOS this also tears down SCK —
    // it lives entirely in the helper now, so killing the helper IS the
    // SCStream stop + picker deactivate. The helper's own Drop /
    // signal-on-exit handling releases SCK; we no longer have to drive
    // remove_output_handler / SCContentSharingPicker::set_active from
    // this process (that code moved into `pollis-capture-macos`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(t) = reader {
            t.abort();
        }
        if let Some(h) = helper.as_mut() {
            let _ = h.kill().await;
        }
        // A picker-phase helper that was orphaned (e.g. capture failed
        // before consuming the picker session) needs the same kill.
        if let Some(p) = picker.as_mut() {
            let _ = p.child.kill().await;
        }
    }
    #[cfg(target_os = "windows")]
    {
        // 1. Fence the WGC callback from touching the source (pairs with
        //    the Acquire load in on_frame_arrived). Only this session's
        //    flag — taken from state — is flipped. This is also what ends
        //    the capture: the next frame callback observes it and calls
        //    InternalCaptureControl::stop(), which unblocks the dedicated
        //    thread's start() and lets it return.
        if let Some(active) = &windows_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
        // 2. Detach the capture thread rather than force-joining it. The
        //    fence above guarantees it can no longer touch the LiveKit
        //    source, so it's safe to unpublish/drop the source below
        //    without waiting; the thread tears down its own WGC + COM
        //    state and exits on the next frame. Joining here would risk
        //    blocking stop indefinitely if the captured surface produced
        //    no further frames.
        drop(windows_thread);
    }
    // 3. Unpublish the track before dropping the source. LiveKit's track
    //    teardown can free the source's webrtc backing; doing it in this
    //    order avoids the "unpublish frees backing, handler crashes" race.
    if let (Some(room), Some(track)) = (room, track) {
        let sid = track.sid();
        if let Err(e) = room.local_participant().unpublish_track(&sid).await {
            eprintln!("[screenshare] unpublish error: {e}");
        }
    }
    // 4. Now the source can be dropped safely.
    drop(source_to_drop);
    if let Some(ev) = ev_opt {
        let _ = ev.send(ScreenShareEvent::LocalStopped);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn push_frame(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx: &[u8],
    preview: Option<&dyn RawSink>,
) {
    // libwebrtc + VP8 require even dimensions; libyuv I420 chroma
    // alignment does too. Crop down rather than ever publishing odd
    // dims.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // Convert (BGRx == little-endian ARGB) and cap to 1080p. A 4K /
    // ultrawide monitor would otherwise hit the SW VP8 encoder native.
    let buffer = convert_and_cap(w, h, stride, bgrx);
    let (out_w, out_h) = (buffer.width(), buffer.height());
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    source.capture_frame(&frame);
    if let Some(sink) = preview {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            out_w,
            out_h,
            timestamp_us,
            &frame.buffer,
        );
        let _ = sink.send(bytes);
    }
}

// ── Helper subprocess wire protocol ──────────────────────────────────────
//
// The Format/Frame/Error framing now lives in the single shared
// `pollis-capture-proto` crate (decode: `pollis_capture_proto::read_msg`,
// used by the start path + reader task above). Both the
// `pollis-capture-linux` and `pollis-capture-macos` helpers encode with
// the same crate, so the wire bytes have exactly one definition. The
// hand-rolled `SocketReader` / `HelperMsg` / `MSG_*` that used to live
// here were removed in the issue #281/#283 helper-split refactor — no
// behavior change, the byte layout is identical.

/// Resolve the per-platform capture helper binary. Linux ->
/// `pollis-capture-linux`, macOS -> `pollis-capture-macos`. Both ship as
/// Tauri `externalBin` sidecars next to the main binary in production.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn capture_helper_name() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "pollis-capture-linux"
    }
    #[cfg(target_os = "macos")]
    {
        "pollis-capture-macos"
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn locate_capture_helper() -> Result<std::path::PathBuf> {
    use std::path::PathBuf;

    let helper = capture_helper_name();

    // 1. Explicit override — useful for dev setups with a non-standard
    //    layout.
    if let Ok(p) = std::env::var("POLLIS_CAPTURE_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }

    // 2. Sidecar next to the current executable (this is how we ship in
    //    production — Tauri bundles the helper as an external bin).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // 3. Dev: workspace target dir. We can't be sure of profile, so try
    //    the running binary's profile first (debug if the parent is
    //    debug, otherwise release), then fall back to the other.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    let workspace_root = manifest_dir
        .as_ref()
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    let profiles: &[&str] = if cfg!(debug_assertions) {
        &["debug", "release"]
    } else {
        &["release", "debug"]
    };
    if let Some(root) = workspace_root.as_ref() {
        for profile in profiles {
            let candidate = root.join("target").join(profile).join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Also try a `target/<profile>` relative to CWD — covers
    // `pnpm dev` running from the repo root.
    if let Ok(cwd) = std::env::current_dir() {
        for profile in profiles {
            let candidate = cwd.join("target").join(profile).join(helper);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err(crate::error::Error::Other(anyhow::anyhow!(
        "{helper} helper binary not found; set POLLIS_CAPTURE_BIN or build it with `cargo build -p {helper}`"
    )))
}

// ── Remote video track plumbing (called from voice.rs room loop) ──────────

pub async fn on_remote_video_subscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    eprintln!("[screenshare] remote video subscribed: {track_key}");

    let (events, frames) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };

    let mut stream = NativeVideoStream::new(track.rtc_track());
    let track_key_for_task = track_key.clone();
    let identity_clone = participant_identity.clone();
    let events_for_task = events.clone();
    let task = tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut announced: Option<(u32, u32)> = None;
        // No stall watchdog — when the remote streamer's capture is
        // idle, frames simply stop arriving and our local canvas keeps
        // showing the last paint. The track stays subscribed; the next
        // real frame is rendered when it arrives. Stream ending (track
        // unpublished) exits this loop and RemoteStopped is emitted by
        // the unsubscribe path.
        while let Some(frame) = stream.next().await {
            let i420 = frame.buffer.to_i420();
            let w = i420.width();
            let h = i420.height();
            if announced != Some((w, h)) {
                announced = Some((w, h));
                if let Some(ev) = &events_for_task {
                    let _ = ev.send(ScreenShareEvent::RemoteStarted {
                        track_key: track_key_for_task.clone(),
                        identity: identity_clone.clone(),
                        width: w,
                        height: h,
                    });
                }
            }
            if let Some(sink) = &frames {
                let bytes = pack_frame_bytes(
                    &track_key_for_task,
                    w,
                    h,
                    frame.timestamp_us,
                    &i420,
                );
                let _ = sink.send(bytes);
            }
        }
    });

    let mut ss = state.screenshare.lock().await;
    if let Some(prev) = ss.remote_drain_tasks.insert(track_key, task) {
        prev.abort();
    }
}

pub async fn on_remote_video_unsubscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    let mut ss = state.screenshare.lock().await;
    if let Some(t) = ss.remote_drain_tasks.remove(&track_key) {
        t.abort();
    }
    if let Some(ev) = &ss.events {
        let _ = ev.send(ScreenShareEvent::RemoteStopped { track_key });
    }
}

pub async fn on_room_disconnected(state: &Arc<AppState>) {
    let _ = stop_screen_share(state).await;
    let mut ss = state.screenshare.lock().await;
    for (_, t) in ss.remote_drain_tasks.drain() {
        t.abort();
    }
}

// ── Frame wire format (Rust -> webview) ───────────────────────────────────
//
// [ u32 LE track_key_len ][ track_key UTF-8 ]
// [ u32 LE width ][ u32 LE height ]
// [ u32 LE y_stride ][ u32 LE u_stride ][ u32 LE v_stride ]
// [ i64 LE timestamp_us ]
// [ Y plane bytes ][ U plane bytes ][ V plane bytes ]
fn pack_frame_bytes(
    track_key: &str,
    width: u32,
    height: u32,
    timestamp_us: i64,
    i420: &I420Buffer,
) -> Vec<u8> {
    let (y_stride, u_stride, v_stride) = i420.strides();
    let (y, u, v) = i420.data();
    let header_len = 4 + track_key.len() + 4 + 4 + 4 + 4 + 4 + 8;
    let mut out = Vec::with_capacity(header_len + y.len() + u.len() + v.len());
    out.extend_from_slice(&(track_key.len() as u32).to_le_bytes());
    out.extend_from_slice(track_key.as_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&y_stride.to_le_bytes());
    out.extend_from_slice(&u_stride.to_le_bytes());
    out.extend_from_slice(&v_stride.to_le_bytes());
    out.extend_from_slice(&timestamp_us.to_le_bytes());
    out.extend_from_slice(y);
    out.extend_from_slice(u);
    out.extend_from_slice(v);
    out
}
