//! Screen-share over LiveKit.
//!
//! Capture path (Linux):
//!   start_screen_share -> bind a Unix socket on a unique tmp path ->
//!   spawn the `pollis-capture-linux` helper subprocess passing the
//!   socket path -> helper opens the xdg-desktop-portal screencast
//!   picker, opens the pipewire stream, and writes BGRx frames to the
//!   socket -> we read the negotiated format, create a LiveKit
//!   NativeVideoSource + LocalVideoTrack and publish it -> a tokio
//!   reader task pulls frames off the socket, runs libyuv argb_to_i420,
//!   and feeds them into the source.
//!
//! Why a subprocess: pulling libpipewire into the same process as
//! libwebrtc + cpal + webkit2gtk + ashpd reliably crashes inside
//! `pw_init`. Isolating capture in its own process makes the linkage
//! soup the kernel's problem.
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
    RemoteStarted {
        track_key: String,
        identity: String,
        width: u32,
        height: u32,
    },
    RemoteStopped { track_key: String },
}

// ── Top-level state ───────────────────────────────────────────────────────

pub struct ScreenShareState {
    pub events: Option<Arc<dyn EventSink<ScreenShareEvent>>>,
    pub frames: Option<Arc<dyn RawSink>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,
    /// Handle to the capture helper subprocess. Dropping/killing it
    /// terminates capture.
    pub local_helper: Option<tokio::process::Child>,
    /// The supervising task that reads from the helper's socket and
    /// pushes frames into the LiveKit source.
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

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
            local_helper: None,
            local_reader_task: None,
            remote_drain_tasks: std::collections::HashMap::new(),
        }
    }
}

/// Trait wrapper for the raw-bytes Channel sink so pollis-core stays
/// free of a tauri runtime dependency. The src-tauri shim adapts a
/// `tauri::ipc::Channel<InvokeResponseBody>` into this.
pub trait RawSink: Send + Sync {
    fn send(&self, bytes: Vec<u8>) -> Result<()>;
}

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

#[cfg(not(target_os = "linux"))]
pub async fn start_screen_share(_state: &Arc<AppState>) -> Result<()> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "screen share is only implemented on Linux right now"
    )))
}

#[cfg(target_os = "linux")]
pub async fn start_screen_share(state: &Arc<AppState>) -> Result<()> {
    use tokio::net::UnixListener;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // 1. Pick a socket path and bind. Filesystem path (not abstract) so
    //    we can clean it up explicitly on stop.
    let socket_path = std::env::temp_dir().join(format!(
        "pollis-capture-{}-{}.sock",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    // Defensive: remove any stale socket at this path.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| anyhow::anyhow!("bind unix socket: {e}"))?;

    // 2. Spawn the helper.
    let helper_path = locate_helper_binary()?;
    eprintln!(
        "[screenshare] spawning helper {} on socket {}",
        helper_path.display(),
        socket_path.display()
    );
    let mut helper = tokio::process::Command::new(&helper_path)
        .arg("--socket")
        .arg(&socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn {}: {e}", helper_path.display()))?;

    // 3. Wait for the helper to connect (or to die without connecting).
    //    The portal can take a while if the user takes their time
    //    picking — give them 5 minutes before we give up.
    let accept_fut = listener.accept();
    let (stream, _addr) = tokio::select! {
        res = accept_fut => {
            let r = res.map_err(|e| anyhow::anyhow!("accept: {e}"))?;
            eprintln!("[screenshare] helper connected");
            r
        }
        status = helper.wait() => {
            let _ = std::fs::remove_file(&socket_path);
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "capture helper exited before connecting: {:?}",
                status
            )));
        }
    };
    // Once connected we can unlink the socket — the open fd keeps the
    // session alive and there's nothing more to bind to.
    let _ = std::fs::remove_file(&socket_path);

    // Park the helper handle in state immediately so a concurrent
    // `stop_screen_share` (e.g. user clicked stop while waiting for
    // the picker, or the publish below errors and they cancel) can
    // kill it via the standard cleanup path.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_helper = Some(helper);
    }

    // 4. Read the first protocol message; expect Format. Anything
    //    else (or EOF) is a hard failure. Generous 5-min timeout
    //    covers the user staring at the picker.
    let mut reader = SocketReader::new(stream);
    eprintln!("[screenshare] awaiting video format from helper");
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        reader.read_message(),
    )
    .await;
    let msg = match read_result {
        Ok(r) => r,
        Err(_) => {
            stop_screen_share(state).await.ok();
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "capture helper: timed out waiting for video format"
            )));
        }
    };
    let (width, height) = match msg {
        Ok(Some(HelperMsg::Format { width, height })) => {
            eprintln!("[screenshare] helper announced {}x{}", width, height);
            (width & !1, height & !1)
        }
        Ok(Some(HelperMsg::Frame { .. })) => {
            stop_screen_share(state).await.ok();
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "capture helper sent video frame before format"
            )));
        }
        Ok(Some(HelperMsg::Error { message })) => {
            stop_screen_share(state).await.ok();
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "capture helper: {message}"
            )));
        }
        Ok(None) => {
            stop_screen_share(state).await.ok();
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "capture helper closed socket before sending video format"
            )));
        }
        Err(e) => {
            stop_screen_share(state).await.ok();
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "capture helper read error: {e}"
            )));
        }
    };

    if width == 0 || height == 0 {
        stop_screen_share(state).await.ok();
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "capture helper announced zero-size format"
        )));
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
        stop_screen_share(state).await.ok();
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "publish screenshare: {e}"
        )));
    }
    eprintln!("[screenshare] track published");

    // 6. Spawn the supervising reader task. It owns the socket + the
    //    LiveKit source from here on; on EOF / error it just exits and
    //    relies on stop_screen_share for the rest of cleanup.
    let source_for_task = source.clone();
    let events_for_task = {
        let ss = state.screenshare.lock().await;
        ss.events.clone()
    };
    let reader_task = tokio::spawn(async move {
        loop {
            match reader.read_message().await {
                Ok(Some(HelperMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                })) => {
                    push_frame(&source_for_task, width, height, stride, timestamp_us, &bgrx);
                }
                Ok(Some(HelperMsg::Format { .. })) => {
                    // Renegotiation mid-stream — currently unsupported,
                    // but harmless to ignore. The next frame will use
                    // the new dimensions; LiveKit's NativeVideoSource
                    // tolerates per-frame size changes.
                }
                Ok(Some(HelperMsg::Error { message })) => {
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

pub async fn stop_screen_share(state: &Arc<AppState>) -> Result<()> {
    let (room, track, mut helper, reader, ev_opt) = {
        let mut ss = state.screenshare.lock().await;
        let track = ss.local_track.take();
        let helper = ss.local_helper.take();
        let reader = ss.local_reader_task.take();
        ss.local_source = None;
        let voice = state.voice.lock().await;
        (
            voice.room.clone(),
            track,
            helper,
            reader,
            ss.events.clone(),
        )
    };

    if let Some(t) = reader {
        t.abort();
    }
    if let Some(h) = helper.as_mut() {
        let _ = h.kill().await;
    }
    if let (Some(room), Some(track)) = (room, track) {
        let sid = track.sid();
        if let Err(e) = room.local_participant().unpublish_track(&sid).await {
            eprintln!("[screenshare] unpublish error: {e}");
        }
    }
    if let Some(ev) = ev_opt {
        let _ = ev.send(ScreenShareEvent::LocalStopped);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn push_frame(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx: &[u8],
) {
    // libwebrtc + VP8 require even dimensions; libyuv I420 chroma
    // alignment does too. Crop down rather than ever publishing odd
    // dims.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    let mut buffer = I420Buffer::new(w as u32, h as u32);
    {
        let (sy, su, sv) = buffer.strides();
        let (dy, du, dv) = buffer.data_mut();
        libwebrtc::native::yuv_helper::argb_to_i420(
            bgrx, stride, dy, sy, du, su, dv, sv, w, h,
        );
    }
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    source.capture_frame(&frame);
}

// ── Helper subprocess wire protocol ──────────────────────────────────────

#[cfg(target_os = "linux")]
const MSG_FORMAT: u8 = 0x01;
#[cfg(target_os = "linux")]
const MSG_FRAME: u8 = 0x02;
#[cfg(target_os = "linux")]
const MSG_ERROR: u8 = 0xFF;

#[cfg(target_os = "linux")]
enum HelperMsg {
    Format { width: u32, height: u32 },
    Frame {
        width: u32,
        height: u32,
        stride: u32,
        timestamp_us: i64,
        bgrx: Vec<u8>,
    },
    Error { message: String },
}

#[cfg(target_os = "linux")]
struct SocketReader {
    inner: tokio::io::BufReader<tokio::net::UnixStream>,
}

#[cfg(target_os = "linux")]
impl SocketReader {
    fn new(stream: tokio::net::UnixStream) -> Self {
        // 64KB read buffer is enough headroom for the small messages
        // (frame headers are ~25 bytes, payload is read direct).
        Self {
            inner: tokio::io::BufReader::with_capacity(64 * 1024, stream),
        }
    }

    async fn read_message(&mut self) -> std::io::Result<Option<HelperMsg>> {
        use tokio::io::AsyncReadExt;
        let mut header = [0u8; 5];
        match self.inner.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let msg_type = header[0];
        let payload_len =
            u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        // Hard cap: 32MB. 8K BGRx frame (~127MB) is far above what we'd
        // ever ship; if we see something bigger it's a desync.
        if payload_len > 32 * 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("oversized helper message: {payload_len}"),
            ));
        }
        match msg_type {
            MSG_FORMAT => {
                if payload_len != 8 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "format payload != 8",
                    ));
                }
                let mut buf = [0u8; 8];
                self.inner.read_exact(&mut buf).await?;
                Ok(Some(HelperMsg::Format {
                    width: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
                    height: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
                }))
            }
            MSG_FRAME => {
                if payload_len < 4 + 4 + 4 + 8 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "frame payload too short",
                    ));
                }
                let mut head = [0u8; 4 + 4 + 4 + 8];
                self.inner.read_exact(&mut head).await?;
                let width = u32::from_le_bytes(head[0..4].try_into().unwrap());
                let height = u32::from_le_bytes(head[4..8].try_into().unwrap());
                let stride = u32::from_le_bytes(head[8..12].try_into().unwrap());
                let timestamp_us = i64::from_le_bytes(head[12..20].try_into().unwrap());
                let body_len = payload_len - head.len();
                let mut bgrx = vec![0u8; body_len];
                self.inner.read_exact(&mut bgrx).await?;
                Ok(Some(HelperMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                }))
            }
            MSG_ERROR => {
                let mut bytes = vec![0u8; payload_len];
                self.inner.read_exact(&mut bytes).await?;
                let message = String::from_utf8_lossy(&bytes).into_owned();
                Ok(Some(HelperMsg::Error { message }))
            }
            other => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown helper msg type: 0x{other:02x}"),
            )),
        }
    }
}

#[cfg(target_os = "linux")]
fn locate_helper_binary() -> Result<std::path::PathBuf> {
    use std::path::PathBuf;

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
            let candidate = dir.join("pollis-capture-linux");
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
            let candidate = root.join("target").join(profile).join("pollis-capture-linux");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // Also try a `target/<profile>` relative to CWD — covers
    // `pnpm dev` running from the repo root.
    if let Ok(cwd) = std::env::current_dir() {
        for profile in profiles {
            let candidate = cwd.join("target").join(profile).join("pollis-capture-linux");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err(crate::error::Error::Other(anyhow::anyhow!(
        "pollis-capture-linux helper binary not found; set POLLIS_CAPTURE_BIN or build it with `cargo build -p pollis-capture-linux`"
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
