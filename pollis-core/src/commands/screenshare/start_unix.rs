//! Linux + macOS capture lifecycle: enumerate sources (macOS in-app
//! picker), cancel an in-flight picker session, start a capture (parks
//! the helper handle in state and spawns the reader-drain task), and the
//! `push_frame` callback that converts each helper BGRx frame into an
//! I420 LiveKit frame.
//!
//! Linux uses xdg-desktop-portal (handled inside `pollis-capture-linux`),
//! so `enumerate_screen_sources` returns empty and the frontend skips its
//! in-app picker — the portal dialog IS the picker. macOS calls
//! `enumerate_screen_sources` first, the frontend renders the in-app
//! picker, then sends the selection back via `start_screen_share(Some(sel))`.

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
    fail_capture,
    helper_subprocess::spawn_and_accept_helper,
    stop::stop_screen_share,
    RawSink, ScreenShareEvent, LOCAL_PREVIEW_KEY, PREVIEW_MIN_INTERVAL,
};

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

#[cfg(target_os = "linux")]
pub async fn enumerate_screen_sources(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    // The system portal (Linux) handles source selection. The frontend
    // should not call this on Linux — but returning an empty list is a
    // safer no-op than an error if it ever does.
    Ok(pollis_capture_proto::SourceList {
        displays: Vec::new(),
        windows: Vec::new(),
    })
}

/// Discard a parked picker session — used when the user backs out of
/// the in-app picker without selecting a source.
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
        // The Linux pipewire helper can deliver a Frame ahead of (or
        // instead of) the Format message: `param_changed` may fire first
        // with a not-yet-negotiated zero size — skipping the Format send —
        // while `process` then streams frames carrying real dimensions.
        // Frames are self-describing, so treat a leading Frame as the
        // format announcement and drop its single payload; the next frame
        // (~1 refresh later) feeds the publish loop normally. (spike/
        // tauri-revival — this path went unexercised through the Electron era.)
        Ok(Some(CaptureMsg::Frame { width, height, .. })) => {
            eprintln!(
                "[screenshare] helper sent frame before format; deriving {}x{}",
                width, height
            );
            (width & !1, height & !1)
        }
        Ok(Some(CaptureMsg::Sources(_))) | Ok(Some(CaptureMsg::Select(_))) => {
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
                video_codec: pick_screenshare_codec(),
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
    // Local self-preview now rides the same WebSocket broadcast the remote
    // tiles use (spike/tauri-revival); without this the sharer's own preview
    // tile stays black, since the frontend stopped listening to the IPC frame
    // Channel when it switched to the WS transport.
    let preview_tx = state.screenshare_frame_tx.clone();
    let (events_for_task, frames_for_task) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };
    let reader_task = tokio::spawn(async move {
        let mut last_preview: Option<std::time::Instant> = None;
        // No FPS cap: pipewire delivers at the source's native refresh
        // (144Hz+ on high-refresh displays) and we publish at the same
        // rate. The SW encoder absorbs that fine on modern hardware; if
        // a future thermal complaint surfaces, the right answer is a
        // user-facing toggle (issue #300), not a hardcoded limit.
        //
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
                    // Throttle self-preview to PREVIEW_MIN_INTERVAL; the same
                    // gate feeds both the legacy RawSink and the WS broadcast.
                    let show_preview = last_preview
                        .map_or(true, |t| t.elapsed() >= PREVIEW_MIN_INTERVAL);
                    if show_preview {
                        last_preview = Some(std::time::Instant::now());
                    }
                    let preview = if show_preview {
                        frames_for_task.as_ref().map(|s| s.as_ref())
                    } else {
                        None
                    };
                    let preview_ws = if show_preview { Some(&preview_tx) } else { None };
                    push_frame(
                        &source_for_task,
                        width,
                        height,
                        stride,
                        timestamp_us,
                        &bgrx,
                        preview,
                        preview_ws,
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

fn push_frame(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx: &[u8],
    preview: Option<&dyn RawSink>,
    preview_ws: Option<&tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>>,
) {
    // libwebrtc + VP8 require even dimensions; libyuv I420 chroma
    // alignment does too. Crop down rather than ever publishing odd
    // dims.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // Convert (BGRx == little-endian ARGB) to I420 at native res.
    let buffer = convert_to_i420(w, h, stride, bgrx);
    let (out_w, out_h) = (buffer.width(), buffer.height());
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    source.capture_frame(&frame);
    if preview.is_some() || preview_ws.is_some() {
        let bytes = pack_frame_bytes(
            LOCAL_PREVIEW_KEY,
            out_w,
            out_h,
            timestamp_us,
            &frame.buffer,
        );
        let bytes = std::sync::Arc::new(bytes);
        if let Some(tx) = preview_ws {
            let _ = tx.send(bytes.clone());
        }
        if let Some(sink) = preview {
            let _ = sink.send((*bytes).clone());
        }
    }
}
