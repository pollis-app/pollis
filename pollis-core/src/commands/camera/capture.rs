//! Webcam capture lifecycle (the parent/host side): enumerate cameras,
//! start a capture (publish a `TrackSource::Camera` track into the active
//! voice room and spawn the reader-drain task), and stop. Mirrors the
//! screen-share `start_unix` model, reusing the shared helper-location and
//! `argb_to_i420` primitives — but with its own state, events, publish
//! options, and a camera-tuned encoding.
//!
//! Platform-agnostic: this talks to the per-OS capture helper purely over
//! the `pollis-capture-proto` Unix-socket protocol, so the same code drives
//! `pollis-capture-macos --mode camera` (AVFoundation) and
//! `pollis-capture-linux --mode camera` (V4L2). `locate_capture_helper`
//! resolves the right helper binary per platform.

use std::sync::Arc;

use libwebrtc::{
    prelude::{RtcVideoSource, VideoBuffer, VideoFrame, VideoRotation},
    video_source::{native::NativeVideoSource, VideoResolution},
};
use livekit::{
    options::{TrackPublishOptions, VideoCodec, VideoEncoding},
    prelude::*,
    track::{LocalTrack, LocalVideoTrack},
};

use crate::commands::screenshare::{
    codec::{convert_to_i420, pack_frame_bytes},
    helper_subprocess::locate_capture_helper,
    HelperSession,
};
use crate::{error::Result, state::AppState};

use super::{fail_capture, CameraEvent, CAMERA_PREVIEW_MIN_INTERVAL, LOCAL_CAMERA_PREVIEW_KEY};

/// How long to wait for the helper's `Cameras` enumeration / `Format`.
/// Generous to cover the macOS camera TCC permission prompt, which blocks
/// the helper's first device-open until the user responds.
const HELPER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Spawn `pollis-capture-macos --mode camera` and wait for it to connect
/// back over a fresh Unix socket. Returns the session split into
/// read/write halves (the parent sends `SelectCamera` and reads
/// `Format`/`Frame`).
async fn spawn_camera_helper(state: &Arc<AppState>) -> Result<HelperSession> {
    use tokio::net::UnixListener;

    let socket_path = std::env::temp_dir().join(format!(
        "pollis-camera-{}-{}.sock",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[camera] bind unix socket: {e}");
            return Err(fail_capture(
                state,
                "Could not set up the camera-capture channel. Please try again.".into(),
            )
            .await);
        }
    };

    let helper_path = match locate_capture_helper() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[camera] locate helper: {e}");
            return Err(fail_capture(
                state,
                "Camera-capture helper not found. Reinstall Pollis or rebuild the capture helper."
                    .into(),
            )
            .await);
        }
    };
    eprintln!(
        "[camera] spawning helper {} (camera mode) on socket {}",
        helper_path.display(),
        socket_path.display()
    );
    let helper = tokio::process::Command::new(&helper_path)
        .arg("--socket")
        .arg(&socket_path)
        .arg("--mode")
        .arg("camera")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        // Pipe stderr (not inherit) so a startup failure — bad args, camera TCC
        // denial, no camera — can be surfaced to the UI instead of vanishing
        // into the terminal. We re-echo it below so dev logs are unchanged.
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut helper = match helper {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[camera] spawn {}: {e}", helper_path.display());
            let _ = std::fs::remove_file(&socket_path);
            return Err(fail_capture(
                state,
                "Could not launch the camera-capture helper. Please try again.".into(),
            )
            .await);
        }
    };

    // Drain the helper's stderr into a buffer (still echoing each line to our
    // own stderr so terminal/dev logs are unchanged). On an early-exit failure
    // this lets us report the helper's actual reason; on success the task keeps
    // draining for the helper's lifetime so its pipe never fills and blocks.
    let stderr_buf = Arc::new(tokio::sync::Mutex::new(String::new()));
    let stderr_task = helper.stderr.take().map(|stderr| {
        let buf = Arc::clone(&stderr_buf);
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[camera-helper] {line}");
                let mut b = buf.lock().await;
                b.push_str(&line);
                b.push('\n');
            }
        })
    });

    let (stream, _addr) = tokio::select! {
        res = listener.accept() => match res {
            Ok(r) => {
                eprintln!("[camera] helper connected");
                r
            }
            Err(e) => {
                eprintln!("[camera] accept: {e}");
                let _ = std::fs::remove_file(&socket_path);
                return Err(fail_capture(
                    state,
                    "Camera-capture helper failed to connect. Please try again.".into(),
                )
                .await);
            }
        },
        status = helper.wait() => {
            eprintln!("[camera] helper exited before connecting: {status:?}");
            let _ = std::fs::remove_file(&socket_path);
            // Give the drain a beat to flush, then surface the helper's real reason.
            if let Some(t) = stderr_task {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(500), t).await;
            }
            let captured = stderr_buf.lock().await;
            let detail = captured.trim();
            let msg = if detail.is_empty() {
                "Camera capture could not start (helper exited). Check camera permission and try again.".to_string()
            } else {
                // The helper's last stderr line is the actual cause (bad args,
                // permission denied, no camera device, …).
                format!(
                    "Camera capture failed: {}",
                    detail.lines().last().unwrap_or(detail)
                )
            };
            return Err(fail_capture(state, msg).await);
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

/// Spawn the helper, let it enumerate cameras via AVFoundation, and return
/// the list. The helper stays parked in `state.camera.picker_session`
/// waiting for the upcoming `start_camera(device_id)`.
pub async fn list_video_devices(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::CameraList> {
    use pollis_capture_proto::{read_msg, CaptureMsg};

    // Discard any previous enumeration session that was never started.
    {
        let prev = {
            let mut cam = state.camera.lock().await;
            cam.picker_session.take()
        };
        if let Some(mut session) = prev {
            let _ = session.child.kill().await;
        }
    }

    let mut session = spawn_camera_helper(state).await?;

    let read = tokio::time::timeout(HELPER_TIMEOUT, read_msg(&mut session.reader)).await;
    let list = match read {
        Ok(Ok(Some(CaptureMsg::Cameras(list)))) => list,
        Ok(Ok(Some(CaptureMsg::Error { message }))) => {
            let _ = session.child.kill().await;
            return Err(fail_capture(
                state,
                format!("Could not list cameras: {message}"),
            )
            .await);
        }
        Ok(Ok(Some(_other))) => {
            let _ = session.child.kill().await;
            return Err(fail_capture(
                state,
                "Camera helper sent an unexpected message during enumeration.".into(),
            )
            .await);
        }
        Ok(Ok(None)) => {
            let _ = session.child.kill().await;
            return Err(fail_capture(
                state,
                "Camera helper exited before listing devices.".into(),
            )
            .await);
        }
        Ok(Err(e)) => {
            let _ = session.child.kill().await;
            return Err(fail_capture(state, format!("Camera enumeration read error: {e}")).await);
        }
        Err(_) => {
            let _ = session.child.kill().await;
            return Err(fail_capture(
                state,
                "Camera enumeration timed out. Please try again.".into(),
            )
            .await);
        }
    };

    eprintln!("[camera] enumerated {} camera(s)", list.cameras.len());
    {
        let mut cam = state.camera.lock().await;
        cam.picker_session = Some(session);
    }
    Ok(list)
}

pub async fn start_camera(state: &Arc<AppState>, device_id: String) -> Result<()> {
    use pollis_capture_proto::{encode_select_camera, read_msg, CameraSelection, CaptureMsg};
    use tokio::io::AsyncWriteExt;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Re-start from a clean slate: tear down any lingering helper/reader/
    // track from a previous camera session.
    {
        let has_prev = {
            let cam = state.camera.lock().await;
            cam.local_track.is_some()
                || cam.local_helper.is_some()
                || cam.local_reader_task.is_some()
        };
        if has_prev {
            let _ = stop_camera(state).await;
        }
    }

    // Reuse the parked enumeration session if `list_video_devices` left one;
    // otherwise spawn fresh and skip past its `Cameras` message.
    let parked = {
        let mut cam = state.camera.lock().await;
        cam.picker_session.take()
    };
    let mut session = match parked {
        Some(s) => s,
        None => {
            let mut s = spawn_camera_helper(state).await?;
            // Drain the leading Cameras enumeration we didn't ask for.
            match tokio::time::timeout(HELPER_TIMEOUT, read_msg(&mut s.reader)).await {
                Ok(Ok(Some(CaptureMsg::Cameras(_)))) => {}
                Ok(Ok(Some(CaptureMsg::Error { message }))) => {
                    let _ = s.child.kill().await;
                    return Err(fail_capture(state, format!("Camera error: {message}")).await);
                }
                _ => {
                    let _ = s.child.kill().await;
                    return Err(fail_capture(
                        state,
                        "Camera helper did not enumerate before selection.".into(),
                    )
                    .await);
                }
            }
            s
        }
    };

    // Send the pick. The helper is parked between Cameras and Format;
    // SelectCamera unblocks it into opening the device.
    if let Err(e) = session
        .writer
        .write_all(&encode_select_camera(&CameraSelection {
            id: device_id.clone(),
        }))
        .await
    {
        eprintln!("[camera] send SelectCamera: {e}");
        let _ = session.child.kill().await;
        return Err(fail_capture(
            state,
            "Could not deliver the camera selection to the helper. Please try again.".into(),
        )
        .await);
    }
    let _ = session.writer.flush().await;

    // Park the helper handle + writer so a concurrent `stop_camera` can
    // tear it down via the standard path. The writer stays open for the
    // capture's lifetime so the helper's read side doesn't see EOF.
    let mut reader = session.reader;
    {
        let mut cam = state.camera.lock().await;
        cam.local_helper = Some(session.child);
        cam.local_writer = Some(session.writer);
    }

    // Read the negotiated Format (or a leading self-describing Frame).
    let read = tokio::time::timeout(HELPER_TIMEOUT, read_msg(&mut reader)).await;
    let (width, height) = match read {
        Ok(Ok(Some(CaptureMsg::Format { width, height }))) => (width & !1, height & !1),
        Ok(Ok(Some(CaptureMsg::Frame { width, height, .. }))) => (width & !1, height & !1),
        Ok(Ok(Some(CaptureMsg::Error { message }))) => {
            stop_camera(state).await.ok();
            let lower = message.to_lowercase();
            if lower.contains("permission") || lower.contains("denied") {
                return Err(fail_capture(
                    state,
                    "Camera access is blocked. Grant Camera permission in System Settings → Privacy & Security, then try again.".into(),
                )
                .await);
            }
            return Err(fail_capture(state, format!("Camera capture failed: {message}")).await);
        }
        Ok(Ok(Some(_other))) => {
            stop_camera(state).await.ok();
            return Err(fail_capture(
                state,
                "Camera capture failed (protocol error). Please try again.".into(),
            )
            .await);
        }
        Ok(Ok(None)) => {
            stop_camera(state).await.ok();
            return Err(fail_capture(
                state,
                "Camera capture ended before it started. Please try again.".into(),
            )
            .await);
        }
        Ok(Err(e)) => {
            stop_camera(state).await.ok();
            return Err(fail_capture(state, format!("Camera read error: {e}")).await);
        }
        Err(_) => {
            stop_camera(state).await.ok();
            return Err(fail_capture(
                state,
                "Camera capture timed out waiting for video. Please try again.".into(),
            )
            .await);
        }
    };

    if width == 0 || height == 0 {
        stop_camera(state).await.ok();
        return Err(fail_capture(
            state,
            "Camera returned an invalid frame size. Please try again.".into(),
        )
        .await);
    }

    // Create the LiveKit track + publish into the voice room.
    // `is_screencast = false`: camera prefers to preserve frame rate over
    // resolution when constrained — the opposite of screen share.
    let source = NativeVideoSource::new(
        VideoResolution { width, height },
        false, /* is_screencast */
    );
    let track =
        LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(source.clone()));
    let (max_framerate, max_bitrate) = resolve_camera_encoding(width, height);
    eprintln!("[camera] publishing camera track {width}x{height} @ {max_framerate}fps");
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Camera,
                video_codec: VideoCodec::VP8,
                video_encoding: Some(VideoEncoding {
                    max_framerate,
                    max_bitrate,
                }),
                ..Default::default()
            },
        )
        .await
    {
        eprintln!("[camera] publish error: {e}");
        stop_camera(state).await.ok();
        return Err(fail_capture(
            state,
            "Could not publish the camera to the call. Check your connection and try again.".into(),
        )
        .await);
    }

    // Spawn the supervising reader task: drains frames off the helper
    // socket and feeds the LiveKit source. Owns the socket + source until
    // EOF / error; the rest of cleanup runs through `stop_camera`.
    //
    // The same broadcast channel screen share uses carries the local
    // self-preview: each frame is mirrored (throttled) to the renderer
    // under `LOCAL_CAMERA_PREVIEW_KEY` so the sharer sees their own webcam.
    let source_for_task = source.clone();
    let preview_tx = state.screenshare_frame_tx.clone();
    let reader_task = tokio::spawn(async move {
        let mut last_preview: Option<std::time::Instant> = None;
        loop {
            match read_msg(&mut reader).await {
                Ok(Some(CaptureMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                })) => {
                    let show_preview = last_preview
                        .map(|t| t.elapsed() >= CAMERA_PREVIEW_MIN_INTERVAL)
                        .unwrap_or(true);
                    if show_preview {
                        last_preview = Some(std::time::Instant::now());
                    }
                    push_frame(
                        Some(&source_for_task),
                        width,
                        height,
                        stride,
                        timestamp_us,
                        &bgrx,
                        show_preview.then_some(&preview_tx),
                    );
                }
                // Mid-stream renegotiation: harmless to ignore, the next
                // frame carries the new dimensions (NativeVideoSource
                // tolerates per-frame size changes).
                Ok(Some(CaptureMsg::Format { .. })) => {}
                // Only valid during the enumeration handshake; ignore once
                // frames are flowing.
                Ok(Some(CaptureMsg::Cameras(_)))
                | Ok(Some(CaptureMsg::SelectCamera(_)))
                | Ok(Some(CaptureMsg::Sources(_)))
                | Ok(Some(CaptureMsg::Select(_))) => {}
                Ok(Some(CaptureMsg::Error { message })) => {
                    eprintln!("[camera] helper error mid-stream: {message}");
                    break;
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[camera] reader error: {e}");
                    break;
                }
            }
        }
    });

    {
        let mut cam = state.camera.lock().await;
        cam.local_source = Some(source);
        cam.local_track = Some(track);
        cam.local_reader_task = Some(reader_task);
        if let Some(ev) = &cam.events {
            let _ = ev.send(CameraEvent::LocalStarted { width, height });
        }
    }
    Ok(())
}

/// Tear down a live (or partially-started) camera capture. Aborts the
/// reader, kills the helper, unpublishes the track, then drops the source.
/// Safe to call when nothing is live — the pre-start teardown relies on it.
pub async fn stop_camera(state: &Arc<AppState>) -> Result<()> {
    let track;
    let source_to_drop;
    let mut helper;
    let reader;
    let mut picker;
    let ev_opt;
    let room;
    {
        let mut cam = state.camera.lock().await;
        track = cam.local_track.take();
        source_to_drop = cam.local_source.take();
        helper = cam.local_helper.take();
        reader = cam.local_reader_task.take();
        picker = cam.picker_session.take();
        // Dropping the writer closes our half so the helper sees EOF and
        // exits even if its parent-death poll is mid-sleep.
        cam.local_writer = None;
        ev_opt = cam.events.clone();
        let voice = state.voice.lock().await;
        room = voice.room.clone();
    }

    let had_session = track.is_some()
        || source_to_drop.is_some()
        || helper.is_some()
        || reader.is_some()
        || picker.is_some();
    if !had_session {
        return Ok(());
    }

    // Abort the reader first, then kill the helper (which stops the
    // AVCaptureSession — it lives entirely in the helper).
    if let Some(t) = reader {
        t.abort();
    }
    if let Some(h) = helper.as_mut() {
        let _ = h.kill().await;
    }
    if let Some(p) = picker.as_mut() {
        let _ = p.child.kill().await;
    }

    // Unpublish before dropping the source: LiveKit's track teardown can
    // free the source's webrtc backing, so this order avoids a
    // use-after-free with any in-flight frame.
    if let (Some(room), Some(track)) = (room, track) {
        let sid = track.sid();
        if let Err(e) = room.local_participant().unpublish_track(&sid).await {
            eprintln!("[camera] unpublish error: {e}");
        }
    }
    drop(source_to_drop);

    if let Some(ev) = ev_opt {
        let _ = ev.send(CameraEvent::LocalStopped);
    }
    Ok(())
}

/// Start a **preview-only** capture (issue #434): drives the local self-preview
/// with NO voice room and NO published track — for the camera picker in Voice &
/// Video settings. Mirrors frames to the renderer under `LOCAL_CAMERA_PREVIEW_KEY`
/// exactly like the in-call self-preview, but never touches LiveKit. Uses its own
/// `preview_*` state slots so an in-call camera is left running undisturbed.
pub async fn start_camera_preview(state: &Arc<AppState>, device_id: String) -> Result<()> {
    use pollis_capture_proto::{encode_select_camera, read_msg, CameraSelection, CaptureMsg};
    use tokio::io::AsyncWriteExt;

    // Restart from a clean slate: tear down any lingering preview capture.
    stop_camera_preview(state).await.ok();

    // Reuse the parked enumeration session `list_video_devices` left, else spawn.
    let parked = {
        let mut cam = state.camera.lock().await;
        cam.picker_session.take()
    };
    let mut session = match parked {
        Some(s) => s,
        None => {
            let mut s = spawn_camera_helper(state).await?;
            match tokio::time::timeout(HELPER_TIMEOUT, read_msg(&mut s.reader)).await {
                Ok(Ok(Some(CaptureMsg::Cameras(_)))) => {}
                Ok(Ok(Some(CaptureMsg::Error { message }))) => {
                    let _ = s.child.kill().await;
                    return Err(fail_capture(state, format!("Camera error: {message}")).await);
                }
                _ => {
                    let _ = s.child.kill().await;
                    return Err(fail_capture(
                        state,
                        "Camera helper did not enumerate before selection.".into(),
                    )
                    .await);
                }
            }
            s
        }
    };

    if let Err(e) = session
        .writer
        .write_all(&encode_select_camera(&CameraSelection { id: device_id.clone() }))
        .await
    {
        eprintln!("[camera] preview send SelectCamera: {e}");
        let _ = session.child.kill().await;
        return Err(fail_capture(
            state,
            "Could not deliver the camera selection to the helper. Please try again.".into(),
        )
        .await);
    }
    let _ = session.writer.flush().await;

    let mut reader = session.reader;
    {
        let mut cam = state.camera.lock().await;
        cam.preview_helper = Some(session.child);
        cam.preview_writer = Some(session.writer);
    }

    // Read the negotiated Format (or a leading self-describing Frame). We only
    // need to know capture started; the preview renderer sizes from each frame's
    // own dimensions, so we don't thread width/height further.
    let read = tokio::time::timeout(HELPER_TIMEOUT, read_msg(&mut reader)).await;
    match read {
        Ok(Ok(Some(CaptureMsg::Format { .. }))) | Ok(Ok(Some(CaptureMsg::Frame { .. }))) => {}
        Ok(Ok(Some(CaptureMsg::Error { message }))) => {
            stop_camera_preview(state).await.ok();
            let lower = message.to_lowercase();
            if lower.contains("permission") || lower.contains("denied") {
                return Err(fail_capture(
                    state,
                    "Camera access is blocked. Grant Camera permission in System Settings → Privacy & Security, then try again.".into(),
                )
                .await);
            }
            return Err(fail_capture(state, format!("Camera preview failed: {message}")).await);
        }
        _ => {
            stop_camera_preview(state).await.ok();
            return Err(fail_capture(
                state,
                "Camera preview could not start. Please try again.".into(),
            )
            .await);
        }
    }

    // Reader task: mirror throttled frames to the local preview ONLY (source =
    // None → no LiveKit publish).
    let preview_tx = state.screenshare_frame_tx.clone();
    let reader_task = tokio::spawn(async move {
        let mut last_preview: Option<std::time::Instant> = None;
        loop {
            match read_msg(&mut reader).await {
                Ok(Some(CaptureMsg::Frame {
                    width,
                    height,
                    stride,
                    timestamp_us,
                    bgrx,
                })) => {
                    let show = last_preview
                        .map(|t| t.elapsed() >= CAMERA_PREVIEW_MIN_INTERVAL)
                        .unwrap_or(true);
                    if show {
                        last_preview = Some(std::time::Instant::now());
                        push_frame(None, width, height, stride, timestamp_us, &bgrx, Some(&preview_tx));
                    }
                }
                Ok(Some(CaptureMsg::Format { .. })) => {}
                Ok(Some(CaptureMsg::Error { message })) => {
                    eprintln!("[camera] preview helper error mid-stream: {message}");
                    break;
                }
                Ok(None) => break,
                Err(e) => {
                    eprintln!("[camera] preview reader error: {e}");
                    break;
                }
                _ => {}
            }
        }
    });
    {
        let mut cam = state.camera.lock().await;
        cam.preview_reader_task = Some(reader_task);
    }
    Ok(())
}

/// Tear down the settings self-preview capture (issue #434). Safe to call when
/// nothing is live — used both on picker close and as the pre-start teardown.
pub async fn stop_camera_preview(state: &Arc<AppState>) -> Result<()> {
    let helper;
    let reader;
    {
        let mut cam = state.camera.lock().await;
        helper = cam.preview_helper.take();
        reader = cam.preview_reader_task.take();
        // Dropping our writer half closes it so the helper sees EOF and exits.
        cam.preview_writer = None;
    }
    if helper.is_none() && reader.is_none() {
        return Ok(());
    }
    if let Some(t) = reader {
        t.abort();
    }
    if let Some(mut h) = helper {
        let _ = h.kill().await;
    }
    Ok(())
}

/// Camera-tuned `(max_framerate, max_bitrate)`. 30fps; bitrate ≈ 0.07
/// bits/pixel/frame (720p ≈ 1.9 Mbps, 1080p ≈ 4.3 Mbps), clamped to a sane
/// band. The encoder treats the bitrate as a ceiling, so a near-static
/// webcam frame costs far less.
fn resolve_camera_encoding(width: u32, height: u32) -> (f64, u64) {
    let max_bitrate =
        ((width as u64) * (height as u64) * 30 * 7 / 100).clamp(500_000, 5_000_000);
    (30.0, max_bitrate)
}

fn push_frame(
    // `Some` feeds the LiveKit publish source; `None` for the preview-only path
    // (settings self-preview with no call / no publish).
    source: Option<&NativeVideoSource>,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx: &[u8],
    // `Some` on the throttled frames that should also feed the sharer's
    // local preview; `None` otherwise. Mirrors screen share's `preview_ws`.
    preview: Option<&tokio::sync::broadcast::Sender<Arc<Vec<u8>>>>,
) {
    // libwebrtc + VP8 + libyuv I420 all require even dimensions.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // BGRA (== little-endian ARGB) → I420 at native resolution.
    let buffer = convert_to_i420(w, h, stride, bgrx);
    let (out_w, out_h) = (buffer.width(), buffer.height());
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer,
    };
    if let Some(source) = source {
        source.capture_frame(&frame);
    }

    // Mirror to the renderer for the local self-preview, reusing the exact
    // I420 buffer just published — same frame wire format + WebSocket
    // transport as the remote tiles and screen-share preview.
    if let Some(tx) = preview {
        let bytes = pack_frame_bytes(
            LOCAL_CAMERA_PREVIEW_KEY,
            out_w,
            out_h,
            timestamp_us,
            &frame.buffer,
        );
        let _ = tx.send(Arc::new(bytes));
    }
}
