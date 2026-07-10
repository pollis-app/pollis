//! Windows webcam capture (Media Foundation) — IN-PROCESS.
//!
//! ── Why in-process (NOT a helper subprocess like macOS/Linux) ─────────────
//!
//! The macOS (`pollis-capture-macos`) and Linux (`pollis-capture-linux`)
//! camera helpers exist because their platforms FORCE isolation:
//!   - macOS: an AVFoundation/CoreMediaIO ObjC `@throw` is uncatchable by
//!     Rust `catch_unwind` and aborts the whole app (#283).
//!   - Linux: libpipewire can't co-link with libwebrtc + cpal + webkit2gtk.
//!
//! Windows has neither hazard: Media Foundation is clean in-process COM that
//! links fine alongside libwebrtc and the webview — exactly like WGC screen
//! capture (`screenshare/start_windows.rs`), which is also in-process. So
//! Windows mirrors the WGC pattern: capture on an owned thread, push frames
//! straight into the LiveKit `NativeVideoSource`, no socket, no proto, no
//! sidecar. See `.codesight/wiki/capture-split.md`.
//!
//! ── Capture flow (mirrors `screenshare/start_windows.rs`) ─────────────────
//!
//!   list_video_devices:
//!     `MFStartup` → `MFCreateAttributes` with
//!     `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE = …_VIDCAP_GUID` →
//!     `MFEnumDeviceSources`. For each `IMFActivate`, read the friendly name
//!     and the `…_VIDCAP_SYMBOLIC_LINK` — the symbolic link IS the opaque
//!     `CameraSource::id` the proto documents, echoed back verbatim to
//!     `start_camera`. Runs on a blocking pool thread (COM + device probe).
//!
//!   start_camera(device_id):
//!     Spawn one owned thread. It re-resolves the device by symbolic link
//!     (`MFCreateDeviceSource`), builds an `IMFSourceReader` with
//!     `MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING` so the reader inserts a
//!     decoder + the Video Processor MFT and hands us **RGB32** (== BGRA ==
//!     the BGRx `convert_to_i420` expects) regardless of the camera's native
//!     format (YUY2 / NV12 / MJPG). It picks the best native resolution
//!     (largest ≤ 1080p) so the color-convert-only processor never has to
//!     resize, reports the negotiated size back over a oneshot, then loops
//!     `ReadSample` → `ConvertToContiguousBuffer` → `Lock` → pack top-down
//!     BGRx → `push_frame` → `Unlock`, gated on the stop fence. The parent
//!     publishes the `TrackSource::Camera` track (VP8, `is_screencast=false`)
//!     only after it has the real dimensions.
//!
//!   stop_camera: flip the fence (`Release`), detach the thread (it tears
//!     down its own MF + COM state on the next loop check), unpublish, drop
//!     the source — the same shape as `screenshare::stop`'s Windows arm.
//!
//! Permission: Windows camera privacy (Settings → Privacy → Camera) makes the
//! device open / first `ReadSample` fail with `E_ACCESSDENIED`; we map that
//! onto the same friendly "permission" error the frontend already shows
//! (`friendlyCameraError` matches "permission"/"denied").
//!
//! Follow-up (not v1): a native-subtype fallback (negotiate YUY2/NV12/MJPG and
//! convert in-process, reusing the Linux helper's converters) for the rare
//! device/driver that refuses RGB32 even through the Video Processor. The
//! RGB32-via-video-processing path here covers every consumer UVC webcam.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result as AnyResult};
use libwebrtc::{
    prelude::{RtcVideoSource, VideoFrame, VideoRotation},
    video_frame::VideoBuffer,
    video_source::{native::NativeVideoSource, VideoResolution},
};
use livekit::{
    options::{TrackPublishOptions, VideoCodec, VideoEncoding},
    prelude::*,
    track::{LocalTrack, LocalVideoTrack},
};
use tokio::sync::broadcast;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

use crate::commands::screenshare::codec::{convert_to_i420, pack_frame_bytes};
use crate::sink::EventSink;
use crate::{error::Result, state::AppState};

use super::{fail_capture, CameraEvent, CAMERA_PREVIEW_MIN_INTERVAL, LOCAL_CAMERA_PREVIEW_KEY};

/// The special stream index that means "first video stream" for the source
/// reader (`MF_SOURCE_READER_FIRST_VIDEO_STREAM` is an i32 newtype; the
/// `ReadSample`/`GetNativeMediaType` calls want the raw `u32`).
const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

// ── Enumeration ────────────────────────────────────────────────────────────

pub async fn list_video_devices(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::CameraList> {
    // MF enumeration is blocking COM work — keep it off the tokio worker.
    let joined = tokio::task::spawn_blocking(enumerate_cameras_blocking)
        .await
        .map_err(|e| anyhow!("camera enumerate join: {e}"));
    match joined {
        Ok(Ok(list)) => {
            eprintln!("[camera] enumerated {} camera(s)", list.cameras.len());
            Ok(list)
        }
        Ok(Err(e)) => Err(fail_capture(state, format!("Could not list cameras: {e}")).await),
        Err(e) => Err(fail_capture(state, format!("Could not list cameras: {e}")).await),
    }
}

/// `MFStartup` → enumerate VIDCAP device sources → `CameraList`. Every device
/// the OS reports, no virtual-camera filtering (Discord/Zoom convention).
fn enumerate_cameras_blocking() -> AnyResult<pollis_capture_proto::CameraList> {
    use pollis_capture_proto::{CameraList, CameraSource};

    let _mf = MfScope::enter()?;

    let mut cameras = Vec::new();
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned null"))?;
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;

        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        MFEnumDeviceSources(&attrs, &mut activates, &mut count)?;

        // Each element is an AddRef'd `IMFActivate`; move it out so its Drop
        // releases the COM ref, then free the CoTaskMemAlloc'd array itself.
        for i in 0..count as usize {
            let activate: Option<IMFActivate> = std::ptr::read(activates.add(i));
            if let Some(activate) = activate {
                let name = get_allocated_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
                    .unwrap_or_else(|| "Camera".to_string());
                // The symbolic link is the opaque, stable device id we echo
                // back verbatim in `start_camera`. A device with no link is
                // unusable, so skip it.
                if let Some(id) = get_allocated_string(
                    &activate,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                ) {
                    cameras.push(CameraSource { id, name });
                }
            }
        }
        if !activates.is_null() {
            CoTaskMemFree(Some(activates as *const c_void));
        }
    }

    Ok(CameraList { cameras })
}

/// Read a `CoTaskMemAlloc`'d wide-string attribute and free it. `None` when the
/// key is absent or the string is empty.
unsafe fn get_allocated_string(
    activate: &IMFActivate,
    key: &windows::core::GUID,
) -> Option<String> {
    let mut pwstr = PWSTR::null();
    let mut len: u32 = 0;
    activate.GetAllocatedString(key, &mut pwstr, &mut len).ok()?;
    if pwstr.is_null() {
        return None;
    }
    let s = pwstr.to_string().ok();
    CoTaskMemFree(Some(pwstr.0 as *const c_void));
    s.filter(|s| !s.is_empty())
}

// ── Capture ────────────────────────────────────────────────────────────────

/// Outcome the dedicated MF thread reports over the oneshot before it enters
/// its streaming loop: the negotiated size, or a genuine setup failure.
enum CamStart {
    Size(u32, u32),
    Failed(String),
}

/// What the capture thread needs. All fields are `Send`; the MF COM objects
/// are created and dropped entirely on the thread, never crossing it.
struct CameraCaptureFlags {
    device_id: String,
    source: NativeVideoSource,
    active: Arc<AtomicBool>,
    frame_tx: broadcast::Sender<Arc<Vec<u8>>>,
}

pub async fn start_camera(state: &Arc<AppState>, device_id: String) -> Result<()> {
    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow!("not in a voice channel — join voice first"))
    })?;

    // Re-start from a clean slate: tear down any lingering thread/track/source
    // from a previous camera session.
    {
        let has_prev = {
            let cam = state.camera.lock().await;
            cam.local_track.is_some() || cam.windows_thread.is_some()
        };
        if has_prev {
            let _ = stop_camera(state).await;
        }
    }

    // Provisional source resolution — the capture thread reports the true
    // negotiated size back over the oneshot before we publish, and
    // NativeVideoSource tolerates per-frame size changes regardless.
    let source = NativeVideoSource::new(
        VideoResolution {
            width: 1280,
            height: 720,
        },
        false, /* is_screencast */
    );

    let active = Arc::new(AtomicBool::new(true));
    let events_for_thread = {
        let cam = state.camera.lock().await;
        cam.events.clone()
    };
    let flags = CameraCaptureFlags {
        device_id,
        source: source.clone(),
        active: Arc::clone(&active),
        frame_tx: state.screenshare_frame_tx.clone(),
    };

    let (size_tx, size_rx) = tokio::sync::oneshot::channel::<CamStart>();
    let capture_thread = std::thread::Builder::new()
        .name("mf-camera".into())
        .spawn(move || run_capture_thread(flags, size_tx, events_for_thread))
        .map_err(|e| anyhow!("spawn mf camera thread: {e}"))?;

    let (width, height) = match size_rx.await {
        Ok(CamStart::Size(w, h)) => (w, h),
        Ok(CamStart::Failed(msg)) => {
            // The thread already reported + is returning; fence so a late
            // frame can't touch the source, then let it drop.
            active.store(false, Ordering::Release);
            drop(capture_thread);
            return Err(fail_capture(state, msg).await);
        }
        Err(_) => {
            active.store(false, Ordering::Release);
            drop(capture_thread);
            return Err(fail_capture(
                state,
                "Camera capture failed to start. Please try again.".into(),
            )
            .await);
        }
    };

    if width == 0 || height == 0 {
        active.store(false, Ordering::Release);
        drop(capture_thread);
        return Err(fail_capture(
            state,
            "Camera returned an invalid frame size. Please try again.".into(),
        )
        .await);
    }

    // Publish the LiveKit track now that the real size is known.
    // `is_screencast = false`: camera prefers to preserve frame rate over
    // resolution when constrained — the opposite of screen share.
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
        active.store(false, Ordering::Release);
        drop(capture_thread);
        return Err(fail_capture(
            state,
            "Could not publish the camera to the call. Check your connection and try again.".into(),
        )
        .await);
    }

    {
        let mut cam = state.camera.lock().await;
        cam.local_source = Some(source);
        cam.local_track = Some(track);
        cam.windows_thread = Some(capture_thread);
        cam.windows_active = Some(active);
        if let Some(ev) = &cam.events {
            let _ = ev.send(CameraEvent::LocalStarted { width, height });
        }
    }
    Ok(())
}

/// Entry point for the dedicated capture thread: init MF, open the device,
/// negotiate RGB32, report the size, then stream until fenced. Any setup
/// failure is reported through `size_tx` so `start_camera`'s await resolves; a
/// mid-stream failure (after the size report) surfaces as `LocalError`.
fn run_capture_thread(
    flags: CameraCaptureFlags,
    size_tx: tokio::sync::oneshot::Sender<CamStart>,
    events: Option<Arc<dyn EventSink<CameraEvent>>>,
) {
    let CameraCaptureFlags {
        device_id,
        source,
        active,
        frame_tx,
    } = flags;

    // Setup phase — MF + COM are torn down by `_mf`'s Drop on every exit path.
    let _mf = match MfScope::enter() {
        Ok(mf) => mf,
        Err(e) => {
            let _ = size_tx.send(CamStart::Failed(friendly_mf_error(&e)));
            return;
        }
    };
    let reader = match unsafe { open_source_reader(&device_id) } {
        Ok(r) => r,
        Err(e) => {
            let _ = size_tx.send(CamStart::Failed(friendly_mf_error(&e)));
            return;
        }
    };
    let (width, height, stride) = match unsafe { negotiate_rgb32(&reader) } {
        Ok(v) => v,
        Err(e) => {
            let _ = size_tx.send(CamStart::Failed(friendly_mf_error(&e)));
            return;
        }
    };
    eprintln!("[camera] MF negotiated {width}x{height} RGB32 stride={stride}");
    let _ = size_tx.send(CamStart::Size(width, height));

    // Streaming phase.
    if let Err(e) = unsafe { stream_frames(&reader, width, height, stride, &source, &active, &frame_tx) } {
        eprintln!("[camera] MF capture error: {e}");
        if let Some(ev) = &events {
            let _ = ev.send(CameraEvent::LocalError {
                message: friendly_mf_error(&e),
            });
        }
    }
    eprintln!("[camera] MF capture thread exiting");
}

/// Re-resolve the device by its symbolic link and build a source reader whose
/// Video Processor is enabled so RGB32 output works on any native format.
unsafe fn open_source_reader(device_id: &str) -> AnyResult<IMFSourceReader> {
    // Device-source attributes: VIDCAP source type + the exact symbolic link.
    let mut dev_attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut dev_attrs, 2)?;
    let dev_attrs = dev_attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned null"))?;
    dev_attrs.SetGUID(
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    )?;
    let link: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    dev_attrs.SetString(
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        PCWSTR(link.as_ptr()),
    )?;

    let media_source = MFCreateDeviceSource(&dev_attrs)
        .map_err(|e| anyhow!("camera no longer available: {e}"))?;

    // Reader attributes: turn on the source reader's Video Processor so it
    // color-converts (and, for MJPG cams, decodes) into our requested RGB32.
    let mut reader_attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut reader_attrs, 1)?;
    let reader_attrs = reader_attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned null"))?;
    reader_attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;

    let reader = MFCreateSourceReaderFromMediaSource(&media_source, &reader_attrs)?;
    Ok(reader)
}

/// Select a native resolution, request RGB32 output at that size (color
/// convert only — no resize, so the size must match a native mode), and read
/// back the negotiated `(width, height, signed_stride)`.
unsafe fn negotiate_rgb32(reader: &IMFSourceReader) -> AnyResult<(u32, u32, i32)> {
    reader.SetStreamSelection(VIDEO_STREAM, true)?;

    let (best_w, best_h) = pick_native_size(reader)?;

    let out = MFCreateMediaType()?;
    out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    out.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(best_w, best_h))?;
    reader
        .SetCurrentMediaType(VIDEO_STREAM, None, &out)
        .map_err(|e| anyhow!("camera does not support RGB32 output: {e}"))?;

    // Read back the type the reader actually settled on.
    let current = reader.GetCurrentMediaType(VIDEO_STREAM)?;
    let (width, height) = unpack_size(current.GetUINT64(&MF_MT_FRAME_SIZE)?);
    // MF_MT_DEFAULT_STRIDE is a LONG stored in a UINT32; a negative value marks
    // a bottom-up (BMP-order) buffer. Absent/zero → assume tightly packed.
    let stride = match current.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
        Ok(0) | Err(_) => width as i32 * 4,
        Ok(s) => s as i32,
    };
    if width == 0 || height == 0 {
        return Err(anyhow!("camera negotiated a zero dimension"));
    }
    Ok((width, height, stride))
}

/// Pick the best native frame size the reader exposes: the largest area that
/// still fits within 1080p (a sane call ceiling), or the smallest available if
/// every mode is larger. Falls back to the reader's current type if the device
/// enumerates no explicit sizes.
unsafe fn pick_native_size(reader: &IMFSourceReader) -> AnyResult<(u32, u32)> {
    let mut best_fit: Option<(u32, u32, u64)> = None;
    let mut smallest: Option<(u32, u32, u64)> = None;

    let mut i = 0u32;
    loop {
        // `GetNativeMediaType` returns MF_E_NO_MORE_TYPES once exhausted.
        let Ok(ty) = reader.GetNativeMediaType(VIDEO_STREAM, i) else {
            break;
        };
        if let Ok(packed) = ty.GetUINT64(&MF_MT_FRAME_SIZE) {
            let (w, h) = unpack_size(packed);
            if w > 0 && h > 0 {
                let area = w as u64 * h as u64;
                if w <= 1920 && h <= 1080 && best_fit.map_or(true, |(_, _, a)| area > a) {
                    best_fit = Some((w, h, area));
                }
                if smallest.map_or(true, |(_, _, a)| area < a) {
                    smallest = Some((w, h, area));
                }
            }
        }
        i += 1;
    }

    if let Some((w, h, _)) = best_fit.or(smallest) {
        return Ok((w, h));
    }

    // No enumerable sizes — fall back to whatever the reader currently reports.
    let current = reader.GetCurrentMediaType(VIDEO_STREAM)?;
    let (w, h) = unpack_size(current.GetUINT64(&MF_MT_FRAME_SIZE)?);
    if w == 0 || h == 0 {
        return Err(anyhow!("camera exposes no usable video format"));
    }
    Ok((w, h))
}

/// Blocking `ReadSample` loop. Packs each RGB32 sample into a top-down BGRx
/// scratch buffer (handling bottom-up DIBs) and pushes it into the LiveKit
/// source until the stop fence is cleared or the stream ends.
unsafe fn stream_frames(
    reader: &IMFSourceReader,
    width: u32,
    height: u32,
    stride: i32,
    source: &NativeVideoSource,
    active: &AtomicBool,
    frame_tx: &broadcast::Sender<Arc<Vec<u8>>>,
) -> AnyResult<()> {
    let row_bytes = width as usize * 4;
    let abs_stride = (stride.unsigned_abs() as usize).max(row_bytes);
    let bottom_up = stride < 0;
    let needed = abs_stride * height as usize;
    // Top-down BGRx scratch reused across frames so the hot loop never
    // reallocates. `convert_to_i420` reads it at `row_bytes` stride.
    let mut scratch = vec![0u8; row_bytes * height as usize];
    let mut last_preview: Option<Instant> = None;

    while active.load(Ordering::Acquire) {
        let mut flags: u32 = 0;
        let mut sample: Option<IMFSample> = None;
        // Synchronous read: blocks until a sample is ready (~one frame). The
        // fence is re-checked at the top of the next iteration.
        reader.ReadSample(
            VIDEO_STREAM,
            0,
            None,
            Some(&mut flags),
            None,
            Some(&mut sample),
        )?;

        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        // A gap / stream tick with no sample this call — keep polling.
        let Some(sample) = sample else {
            continue;
        };
        // Re-check the fence right before touching the source (a stop may have
        // landed while ReadSample was blocked).
        if !active.load(Ordering::Acquire) {
            break;
        }

        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut cur_len: u32 = 0;
        buffer.Lock(&mut ptr, None, Some(&mut cur_len))?;
        let packed = !ptr.is_null() && cur_len as usize >= needed;
        if packed {
            let src = std::slice::from_raw_parts(ptr, cur_len as usize);
            for row in 0..height as usize {
                let src_row = if bottom_up {
                    height as usize - 1 - row
                } else {
                    row
                };
                let s = src_row * abs_stride;
                let d = row * row_bytes;
                scratch[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
            }
        }
        buffer.Unlock()?;
        // Short/empty buffer (warm-up frame before the sensor settles) — skip.
        if !packed {
            continue;
        }

        let timestamp_us = now_us();
        let show_preview = last_preview.map_or(true, |t| t.elapsed() >= CAMERA_PREVIEW_MIN_INTERVAL);
        if show_preview {
            last_preview = Some(Instant::now());
        }
        push_frame(
            source,
            width,
            height,
            row_bytes as u32,
            timestamp_us,
            &scratch,
            show_preview.then_some(frame_tx),
        );
    }
    Ok(())
}

// ── Stop ───────────────────────────────────────────────────────────────────

/// Tear down a live (or partially-started) camera capture. Fences the MF loop,
/// detaches its thread, unpublishes the track, then drops the source. Safe to
/// call when nothing is live — the pre-start teardown relies on it.
pub async fn stop_camera(state: &Arc<AppState>) -> Result<()> {
    let track;
    let source_to_drop;
    let thread;
    let active;
    let ev_opt;
    let room;
    {
        let mut cam = state.camera.lock().await;
        track = cam.local_track.take();
        source_to_drop = cam.local_source.take();
        thread = cam.windows_thread.take();
        active = cam.windows_active.take();
        ev_opt = cam.events.clone();
        let voice = state.voice.lock().await;
        room = voice.room.clone();
    }

    let had_session = track.is_some() || source_to_drop.is_some() || thread.is_some();
    if !had_session {
        return Ok(());
    }

    // 1. Fence the MF loop from touching the source; it observes this at the
    //    top of its next iteration, tears down its own MF + COM state, exits.
    if let Some(active) = &active {
        active.store(false, Ordering::Release);
    }
    // 2. Detach the thread rather than force-joining. The fence guarantees it
    //    can no longer touch the source, and it holds its own source clone —
    //    so the backing stays alive until it exits regardless of the drop
    //    below. Joining could block stop if the device stopped yielding frames.
    drop(thread);
    // 3. Unpublish before dropping the source (LiveKit's track teardown can
    //    free the source's webrtc backing).
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

// ── Frame push (mirrors camera/capture.rs::push_frame) ─────────────────────

fn push_frame(
    source: &NativeVideoSource,
    width: u32,
    height: u32,
    stride: u32,
    timestamp_us: i64,
    bgrx: &[u8],
    // `Some` on the throttled frames that also feed the sharer's self-preview.
    preview: Option<&broadcast::Sender<Arc<Vec<u8>>>>,
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
    source.capture_frame(&frame);

    // Mirror to the renderer for the local self-preview, reusing the exact
    // I420 buffer just published — same wire format + WebSocket transport as
    // the remote tiles and screen-share preview.
    if let Some(tx) = preview {
        let bytes = pack_frame_bytes(LOCAL_CAMERA_PREVIEW_KEY, out_w, out_h, timestamp_us, &frame.buffer);
        let _ = tx.send(Arc::new(bytes));
    }
}

/// Camera-tuned `(max_framerate, max_bitrate)` — identical to the helper-path
/// `camera::capture::resolve_camera_encoding` (which isn't compiled on
/// Windows). 30fps; bitrate ≈ 0.07 bits/pixel/frame, clamped to a sane band.
fn resolve_camera_encoding(width: u32, height: u32) -> (f64, u64) {
    let max_bitrate = ((width as u64) * (height as u64) * 30 * 7 / 100).clamp(500_000, 5_000_000);
    (30.0, max_bitrate)
}

// ── MF / COM helpers ───────────────────────────────────────────────────────

/// RAII guard pairing `CoInitializeEx` + `MFStartup` with their shutdowns so
/// every exit path from the capture thread / enumeration cleans up.
struct MfScope;

impl MfScope {
    fn enter() -> AnyResult<Self> {
        unsafe {
            // MTA: the synchronous source reader + device enumeration are
            // apartment-agnostic. On a fresh thread this always succeeds; a
            // prior init with a different model returns RPC_E_CHANGED_MODE,
            // which we ignore (we never own such a thread here).
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            // Balance the CoInitialize if MFStartup fails — otherwise `Self`
            // is never built, Drop never runs, and a pooled blocking thread
            // (enumeration) leaks an unpaired apartment init.
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
                CoUninitialize();
                return Err(anyhow!("MFStartup: {e}"));
            }
        }
        Ok(Self)
    }
}

impl Drop for MfScope {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

/// Map a raw MF/COM error to the friendly string the frontend understands.
/// `E_ACCESSDENIED` (privacy toggle off / camera in another app's exclusive
/// grip) maps onto the "permission" branch of `friendlyCameraError`.
fn friendly_mf_error(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let lower = s.to_lowercase();
    if lower.contains("0x80070005")
        || lower.contains("access is denied")
        || lower.contains("denied")
    {
        return "Camera access is blocked. Grant camera permission in Windows Settings → Privacy → Camera, then try again.".to_string();
    }
    format!("Camera capture failed: {s}")
}

/// `MF_MT_FRAME_SIZE` packs width in the high 32 bits, height in the low 32.
fn pack_size(width: u32, height: u32) -> u64 {
    ((width as u64) << 32) | height as u64
}

fn unpack_size(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Settings self-preview (issue #434). Windows *capture* is implemented above;
/// only this preview-only path is still pending — it would open the MF reader and
/// mirror to `LOCAL_CAMERA_PREVIEW_KEY` without publishing.
pub async fn start_camera_preview(_state: &Arc<AppState>, _device_id: String) -> Result<()> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "Windows camera preview is not implemented yet (Media Foundation TODO)"
    )))
}

/// Idempotent no-op — no preview session exists on Windows yet.
pub async fn stop_camera_preview(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}
