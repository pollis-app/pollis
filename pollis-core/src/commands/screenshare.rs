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

/// Track key the local outgoing capture is mirrored under so the sharer can
/// watch a low-rate preview of their own stream. Reserved sentinel — never
/// collides with a remote "{identity}-{sid}" key.
pub const LOCAL_PREVIEW_KEY: &str = "__local_preview__";

/// The self-preview answers "is my stream actually going out?", not
/// fidelity. Cap it well below capture rate so the extra I420 pack + IPC
/// stays off the hot path.
const PREVIEW_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

// ── Top-level state ───────────────────────────────────────────────────────

pub struct ScreenShareState {
    pub events: Option<Arc<dyn EventSink<ScreenShareEvent>>>,
    pub frames: Option<Arc<dyn RawSink>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,
    /// Linux: handle to the capture helper subprocess. Dropping/killing it
    /// terminates capture.
    #[cfg(target_os = "linux")]
    pub local_helper: Option<tokio::process::Child>,
    /// Linux: the supervising task that reads from the helper's socket and
    /// pushes frames into the LiveKit source.
    #[cfg(target_os = "linux")]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,
    /// macOS: the live ScreenCaptureKit stream. Dropping it stops capture;
    /// we call stop_capture() explicitly on stop_screen_share so the
    /// teardown is synchronous and ordered with track unpublish.
    #[cfg(target_os = "macos")]
    pub macos_stream: Option<screencapturekit::stream::SCStream>,
    /// macOS: handler id returned by `add_output_handler`. Used to call
    /// `remove_output_handler` explicitly on stop, which tells Swift to
    /// detach the output via `sc_stream_remove_stream_output` rather than
    /// relying on Drop. The crate recommends this; in practice it lets
    /// SCK release its retain on our handler (and our source clone)
    /// before the stream itself is released.
    #[cfg(target_os = "macos")]
    pub macos_handler_id: Option<usize>,
    /// macOS: per-session flag the SCK output handler checks before
    /// dereferencing the LiveKit source. SCK's output dispatch queue can
    /// still fire frames after stop_capture() returns, and the source's
    /// backing may be freed by the unpublish path by then. Flipping this
    /// to false in stop_screen_share is the synchronization point that
    /// stops the handler from touching the source.
    ///
    /// It is `Some` only while a share is live and is a *fresh* Arc per
    /// session — `stop` takes it, so a late/duplicate stop of an old
    /// session can never fence the handler of a newer one (that bug
    /// surfaced as a green screen on the second share).
    #[cfg(target_os = "macos")]
    pub macos_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

    /// Windows: handle to the free-threaded Windows.Graphics.Capture
    /// session. `CaptureControl::stop()` joins the pump thread and ends
    /// capture; held so `stop_screen_share` can tear it down on demand.
    #[cfg(target_os = "windows")]
    pub windows_capture: Option<
        windows_capture::capture::CaptureControl<
            WindowsCaptureHandler,
            Box<dyn std::error::Error + Send + Sync>,
        >,
    >,
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
            #[cfg(target_os = "linux")]
            local_helper: None,
            #[cfg(target_os = "linux")]
            local_reader_task: None,
            #[cfg(target_os = "macos")]
            macos_stream: None,
            #[cfg(target_os = "macos")]
            macos_handler_id: None,
            #[cfg(target_os = "macos")]
            macos_active: None,
            #[cfg(target_os = "windows")]
            windows_capture: None,
            #[cfg(target_os = "windows")]
            windows_active: None,
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

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub async fn start_screen_share(_state: &Arc<AppState>) -> Result<()> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "screen share is not implemented on this OS yet"
    )))
}

// ── macOS path ────────────────────────────────────────────────────────────
//
// In-process via the `screencapturekit` crate. No subprocess needed — Apple's
// framework is a clean linkage on macOS and doesn't fight libwebrtc/cpal/Tauri
// the way Linux's libpipewire does.
//
// Capture flow:
//   1. Enumerate displays via SCShareableContent.
//   2. Build a filter for display[0] (no window exclusions).
//   3. Configure the stream at the display's native dimensions, BGRA pixel
//      format, cursor visible.
//   4. Create the LiveKit NativeVideoSource + LocalVideoTrack, publish to the
//      current voice room as Screenshare/VP8 (matching Linux).
//   5. Build an SCStreamOutputTrait handler that owns a clone of the source
//      and converts each BGRA sample to I420 inline.
//   6. start_capture() and stash the SCStream in state so it isn't dropped.
//
// TCC: the system shows the standard "X wants to record your screen" prompt
// on the first capture attempt; no Info.plist key is needed (Apple doesn't
// support customizing the message for screen recording). If the user denies
// or hasn't granted yet, start_capture() returns an error which we surface
// up as a normal failure.
#[cfg(target_os = "macos")]
pub async fn start_screen_share(state: &Arc<AppState>) -> Result<()> {
    use screencapturekit::content_sharing_picker::{
        SCContentSharingPicker, SCContentSharingPickerConfiguration,
        SCContentSharingPickerMode, SCPickerOutcome,
    };
    use screencapturekit::prelude::*;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Re-share must start from a clean slate. If a previous session is
    // still parked in state (shared → stopped → shares again), fully tear
    // it down first; leftover SCStream / picker state otherwise makes the
    // next capture deliver blank frames that render as a green screen.
    {
        let has_prev = {
            let ss = state.screenshare.lock().await;
            ss.local_track.is_some() || ss.macos_stream.is_some()
        };
        if has_prev {
            let _ = stop_screen_share(state).await;
        }
    }

    // 1. Show the macOS system content-sharing picker. Equivalent to Linux's
    //    xdg-desktop-portal picker — system-modal, lets the user choose a
    //    display, a window, or an app. The picker is non-blocking: show()
    //    returns immediately and Swift fires the callback on the main run
    //    loop when the user makes a selection. Bridge it to async via a
    //    oneshot channel.
    let mut picker_config = SCContentSharingPickerConfiguration::new();
    picker_config.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
        SCContentSharingPickerMode::SingleApplication,
    ]);

    let (tx, rx) = tokio::sync::oneshot::channel::<SCPickerOutcome>();
    SCContentSharingPicker::show(&picker_config, move |outcome| {
        let _ = tx.send(outcome);
    });
    let outcome = rx
        .await
        .map_err(|_| anyhow::anyhow!("screen share picker callback dropped before responding"))?;
    let picked = match outcome {
        SCPickerOutcome::Picked(p) => p,
        SCPickerOutcome::Cancelled => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "screen share cancelled"
            )));
        }
        SCPickerOutcome::Error(msg) => {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "screen share picker error: {msg}"
            )));
        }
    };

    // 2. Build the stream around the picked filter. SCStream::new is a
    //    Swift bridge call so we keep it on a blocking thread.
    let (display_w, display_h, stream) = tokio::task::spawn_blocking(move || -> Result<_> {
        let filter = picked.filter();
        let (px_w, px_h) = picked.pixel_size();
        // Force even dims for VP8 + I420 chroma alignment.
        let width = px_w & !1;
        let height = px_h & !1;
        if width == 0 || height == 0 {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "picker reported zero-size selection"
            )));
        }
        eprintln!("[screenshare] macOS picked {}x{}", width, height);

        let config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(true);

        let stream = SCStream::new(&filter, &config);
        Ok((width, height, stream))
    })
    .await
    .map_err(|e| anyhow::anyhow!("screencapturekit init panicked: {e}"))??;

    // 2. Create LiveKit source + track and publish.
    let source = NativeVideoSource::new(
        VideoResolution {
            width: display_w,
            height: display_h,
        },
        true, /* is_screencast */
    );
    let track = LocalVideoTrack::create_video_track(
        "screenshare",
        RtcVideoSource::Native(source.clone()),
    );
    eprintln!("[screenshare] publishing track {}x{}", display_w, display_h);
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
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "publish screenshare: {e}"
        )));
    }
    eprintln!("[screenshare] track published");

    // 3. Hook the SCStream output to push every BGRA sample into the source.
    //    The handler runs on ScreenCaptureKit's own dispatch queue (not a
    //    tokio worker), so the BGRA→I420 conversion happens off the runtime.
    //    The active flag lets stop_screen_share fence the handler from
    //    touching the source after teardown begins.
    // Fresh per-session flag — owned by this share, taken by its stop. A
    // stale stop of an older session can't reach this one.
    let active_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let frames_sink = {
        let ss = state.screenshare.lock().await;
        ss.frames.clone()
    };
    let handler = MacOsFrameHandler {
        source: source.clone(),
        active: std::sync::Arc::clone(&active_flag),
        frames: frames_sink,
        last_preview: std::sync::Mutex::new(None),
    };

    // 4. start_capture is blocking + may take a moment if TCC is prompting,
    //    so push it off the runtime and surface errors as LocalError.
    let started = tokio::task::spawn_blocking(move || -> Result<(SCStream, Option<usize>)> {
        let mut stream = stream;
        let handler_id = stream.add_output_handler(handler, SCStreamOutputType::Screen);
        stream
            .start_capture()
            .map_err(|e| anyhow::anyhow!("SCStream::start_capture: {e}"))?;
        Ok((stream, handler_id))
    })
    .await
    .map_err(|e| anyhow::anyhow!("screencapturekit start panicked: {e}"))?;

    let (stream, handler_id) = match started {
        Ok(s) => s,
        Err(e) => {
            // Roll back the publish so the track doesn't dangle in the room.
            let sid = track.sid();
            let _ = room.local_participant().unpublish_track(&sid).await;
            return Err(e);
        }
    };

    // 5. Stash everything for stop_screen_share + Drop.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.macos_stream = Some(stream);
        ss.macos_handler_id = handler_id;
        ss.macos_active = Some(std::sync::Arc::clone(&active_flag));
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted {
                width: display_w,
                height: display_h,
            });
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
struct MacOsFrameHandler {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
    /// Last time a self-preview frame was emitted. SCK fires the handler
    /// on its own dispatch queue, so interior mutability via Mutex.
    last_preview: std::sync::Mutex<Option<std::time::Instant>>,
}

#[cfg(target_os = "macos")]
impl screencapturekit::prelude::SCStreamOutputTrait for MacOsFrameHandler {
    fn did_output_sample_buffer(
        &self,
        sample: screencapturekit::prelude::CMSampleBuffer,
        output_type: screencapturekit::prelude::SCStreamOutputType,
    ) {
        use screencapturekit::cv::CVPixelBufferLockFlags;
        use screencapturekit::prelude::SCStreamOutputType;

        if !matches!(output_type, SCStreamOutputType::Screen) {
            return;
        }
        // Bail before touching the source if a stop is in progress. SCK's
        // output queue keeps draining for a moment after stop_capture(),
        // and the source's backing may already be freed by the unpublish
        // path. Acquire ordering pairs with the Release store in stop.
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let Some(pixel_buffer) = sample.image_buffer() else {
            return;
        };
        let Ok(guard) = pixel_buffer.lock(CVPixelBufferLockFlags::READ_ONLY) else {
            return;
        };
        let width = guard.width() as u32;
        let height = guard.height() as u32;
        let stride = guard.bytes_per_row() as u32;
        let bgra = guard.as_slice();
        // CMSampleBuffer presentation timestamps are in CMTime; for now we
        // use a wall-clock μs since this matches what LiveKit downstream
        // surfaces in to_i420 path and is sufficient for screencast.
        let timestamp_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        let preview = match &self.frames {
            Some(sink) => {
                let mut lp = self.last_preview.lock().unwrap();
                if lp.map_or(true, |t| t.elapsed() >= PREVIEW_MIN_INTERVAL) {
                    *lp = Some(std::time::Instant::now());
                    Some(sink.as_ref())
                } else {
                    None
                }
            }
            None => None,
        };
        push_frame_macos(&self.source, width, height, stride, timestamp_us, bgra, preview);
        // Explicitly drop the lock guard before returning so the
        // CVPixelBuffer is unlocked promptly for ScreenCaptureKit.
        drop(guard);
    }
}

#[cfg(target_os = "macos")]
fn push_frame_macos(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgra: &[u8],
    preview: Option<&dyn RawSink>,
) {
    // Same constraint as Linux: VP8 + I420 require even dimensions.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    let mut buffer = I420Buffer::new(w as u32, h as u32);
    {
        let (sy, su, sv) = buffer.strides();
        let (dy, du, dv) = buffer.data_mut();
        // libwebrtc::yuv_helper::argb_to_i420 treats input as little-endian
        // 32-bit ARGB, which in memory is B,G,R,A byte order — identical to
        // BGRA from ScreenCaptureKit. Same call as the Linux BGRx path.
        libwebrtc::native::yuv_helper::argb_to_i420(
            bgra, stride, dy, sy, du, su, dv, sv, w, h,
        );
    }
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    source.capture_frame(&frame);
    if let Some(sink) = preview {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            w as u32,
            h as u32,
            timestamp_us,
            &frame.buffer,
        );
        let _ = sink.send(bytes);
    }
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
    let (events_for_task, frames_for_task) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };
    let reader_task = tokio::spawn(async move {
        let mut last_preview: Option<std::time::Instant> = None;
        loop {
            match reader.read_message().await {
                Ok(Some(HelperMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                })) => {
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
pub async fn start_screen_share(state: &Arc<AppState>) -> Result<()> {
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
            ss.local_track.is_some() || ss.windows_capture.is_some()
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
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "publish screenshare: {e}"
        )));
    }
    eprintln!("[screenshare] track published");

    // 2. Fresh per-session fence + the frames sink for the self-preview.
    let active_flag = std::sync::Arc::new(AtomicBool::new(true));
    let frames_sink = {
        let ss = state.screenshare.lock().await;
        ss.frames.clone()
    };

    // 3. Picker + capture start, all on a blocking thread (the picker
    //    pumps messages; the picked item is not Send).
    let flags = WindowsCaptureFlags {
        source: source.clone(),
        active: std::sync::Arc::clone(&active_flag),
        frames: frames_sink,
    };
    let start_res = tokio::task::spawn_blocking(
        move || -> Result<(
            windows_capture::capture::CaptureControl<
                WindowsCaptureHandler,
                Box<dyn std::error::Error + Send + Sync>,
            >,
            u32,
            u32,
        )> {
            use windows_capture::capture::GraphicsCaptureApiHandler;
            use windows_capture::graphics_capture_picker::GraphicsCapturePicker;
            use windows_capture::settings::{
                ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
                MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
            };

            let picked = match GraphicsCapturePicker::pick_item() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return Err(crate::error::Error::Other(anyhow::anyhow!(
                        "screen share cancelled"
                    )))
                }
                Err(e) => {
                    return Err(crate::error::Error::Other(anyhow::anyhow!(
                        "screen share picker error: {e}"
                    )))
                }
            };
            let (sw, sh) = picked
                .size()
                .map_err(|e| anyhow::anyhow!("picker size: {e}"))?;
            // Force even dims for VP8 + I420 chroma alignment.
            let width = (sw.max(0) as u32) & !1;
            let height = (sh.max(0) as u32) & !1;
            if width == 0 || height == 0 {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "picker reported zero-size selection"
                )));
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
            let control = WindowsCaptureHandler::start_free_threaded(settings)
                .map_err(|e| anyhow::anyhow!("start WGC capture: {e}"))?;
            Ok((control, width, height))
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("windows capture start panicked: {e}"))?;

    let (control, width, height) = match start_res {
        Ok(v) => v,
        Err(e) => {
            // Roll back the publish so the track doesn't dangle.
            let sid = track.sid();
            let _ = room.local_participant().unpublish_track(&sid).await;
            return Err(e);
        }
    };

    // 4. Stash for stop_screen_share + announce.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.windows_capture = Some(control);
        ss.windows_active = Some(std::sync::Arc::clone(&active_flag));
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
struct WindowsCaptureFlags {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
}

#[cfg(target_os = "windows")]
pub struct WindowsCaptureHandler {
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
    let mut buffer = I420Buffer::new(w as u32, h as u32);
    {
        let (sy, su, sv) = buffer.strides();
        let (dy, du, dv) = buffer.data_mut();
        // argb_to_i420 treats input as little-endian 32-bit ARGB == B,G,R,A
        // in memory == WGC Bgra8. Same call as macOS/Linux.
        libwebrtc::native::yuv_helper::argb_to_i420(
            bgra, stride, dy, sy, du, su, dv, sv, w, h,
        );
    }
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    source.capture_frame(&frame);
    if let Some(sink) = preview {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            w as u32,
            h as u32,
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
    #[cfg(target_os = "linux")]
    let mut helper;
    #[cfg(target_os = "linux")]
    let reader;
    #[cfg(target_os = "macos")]
    let macos_stream;
    #[cfg(target_os = "macos")]
    let macos_handler_id;
    #[cfg(target_os = "macos")]
    let macos_active;
    #[cfg(target_os = "windows")]
    let windows_capture;
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
        #[cfg(target_os = "linux")]
        {
            helper = ss.local_helper.take();
            reader = ss.local_reader_task.take();
        }
        #[cfg(target_os = "macos")]
        {
            macos_stream = ss.macos_stream.take();
            macos_handler_id = ss.macos_handler_id.take();
            macos_active = ss.macos_active.take();
        }
        #[cfg(target_os = "windows")]
        {
            windows_capture = ss.windows_capture.take();
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
    #[cfg(target_os = "macos")]
    let had_session = track.is_some() || source_to_drop.is_some() || macos_stream.is_some();
    #[cfg(target_os = "linux")]
    let had_session = track.is_some()
        || source_to_drop.is_some()
        || helper.is_some()
        || reader.is_some();
    #[cfg(target_os = "windows")]
    let had_session =
        track.is_some() || source_to_drop.is_some() || windows_capture.is_some();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let had_session = track.is_some() || source_to_drop.is_some();
    if !had_session {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(t) = reader {
            t.abort();
        }
        if let Some(h) = helper.as_mut() {
            let _ = h.kill().await;
        }
    }
    #[cfg(target_os = "macos")]
    {
        use screencapturekit::content_sharing_picker::SCContentSharingPicker;
        use screencapturekit::prelude::SCStreamOutputType;

        // 1. Fence the handler from touching the source. Release pairs with
        //    Acquire load in did_output_sample_buffer. Only this session's
        //    flag — taken from state — is flipped.
        if let Some(active) = &macos_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
        // 2. Explicitly detach the output handler, stop SCK, drop the
        //    stream. All on a blocking thread — these are Swift FFI calls
        //    that block until SCK acks. remove_output_handler tells Swift
        //    to call sc_stream_remove_stream_output, which releases SCK's
        //    retain on the handler (and its clone of the source).
        if let Some(mut stream) = macos_stream {
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(id) = macos_handler_id {
                    let removed = stream.remove_output_handler(id, SCStreamOutputType::Screen);
                    if !removed {
                        eprintln!("[screenshare] remove_output_handler returned false (id={id})");
                    }
                }
                if let Err(e) = stream.stop_capture() {
                    eprintln!("[screenshare] SCStream::stop_capture: {e}");
                }
                drop(stream);
            })
            .await;
        }
        // 3. Deactivate the system-level content-sharing picker. show()
        //    flipped it to active; without flipping it back, the Control
        //    Center menubar entry stays in "ready to share" state long
        //    after our SCStream is gone — looks like we're still capturing.
        SCContentSharingPicker::set_active(false);
    }
    #[cfg(target_os = "windows")]
    {
        // 1. Fence the WGC callback from touching the source (pairs with
        //    the Acquire load in on_frame_arrived). Only this session's
        //    flag — taken from state — is flipped.
        if let Some(active) = &windows_active {
            active.store(false, std::sync::atomic::Ordering::Release);
        }
        // 2. Stop the capture session. CaptureControl::stop() joins the
        //    WGC pump thread, so run it off the runtime.
        if let Some(control) = windows_capture {
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = control.stop() {
                    eprintln!("[screenshare] WGC CaptureControl::stop: {e}");
                }
            })
            .await;
        }
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

#[cfg(target_os = "linux")]
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
    if let Some(sink) = preview {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            w as u32,
            h as u32,
            timestamp_us,
            &frame.buffer,
        );
        let _ = sink.send(bytes);
    }
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
