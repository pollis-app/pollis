//! Windows capture lifecycle. In-process via the `windows-capture` crate
//! (Windows.Graphics.Capture). Like macOS, no subprocess is needed — WGC
//! is a clean in-proc linkage and doesn't fight libwebrtc/cpal/Tauri the
//! way Linux's libpipewire does.
//!
//! Capture flow (mirrors macOS):
//!   1. Show the system GraphicsCapturePicker (display/window/app).
//!   2. Create the LiveKit NativeVideoSource + LocalVideoTrack, publish to
//!      the current voice room as Screenshare/VP8.
//!   3. start_free_threaded a handler that owns a clone of the source and
//!      converts every BGRA8 WGC frame to I420 inline (off the tokio
//!      runtime — WGC pumps on its own worker thread).
//!   4. Stash the CaptureControl in state so stop is synchronous + ordered
//!      with the track unpublish.
//!
//! The picker + session start run inside one spawn_blocking: the picker
//! pumps a message loop and the picked item is not Send, so it can't cross
//! the await boundary. We publish first (provisional resolution; WGC's
//! real per-frame dimensions drive the stream and LiveKit tolerates a
//! per-frame size change) so no initial frames are lost.

use std::sync::Arc;

use libwebrtc::{
    prelude::{RtcVideoSource, VideoFrame, VideoRotation},
    video_frame::VideoBuffer,
    video_source::{native::NativeVideoSource, VideoResolution},
};
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    track::{LocalTrack, LocalVideoTrack},
};

use crate::{error::Result, state::AppState};

use super::{
    codec::{convert_to_i420, pack_frame_bytes, pick_screenshare_codec},
    fail_capture, RawSink, ScreenShareEvent, LOCAL_PREVIEW_KEY, PREVIEW_MIN_INTERVAL,
};

pub async fn enumerate_screen_sources(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    // The system picker (Windows) handles source selection. The frontend
    // should not call this on Windows — but returning an empty list is a
    // safer no-op than an error if it ever does.
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

pub async fn cancel_screen_share_picker(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}

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
            let _ = super::stop::stop_screen_share(state).await;
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
                video_codec: pick_screenshare_codec(),
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

struct WindowsCaptureFlags {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
}

struct WindowsCaptureHandler {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
    // on_frame_arrived takes &mut self (WGC serializes the callback), so a
    // plain field suffices — no Mutex unlike the macOS &self handler.
    last_preview: Option<std::time::Instant>,
}

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
    // Convert (WGC Bgra8 == little-endian ARGB) to I420 at native res.
    let buffer = convert_to_i420(w, h, stride, bgra);
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
