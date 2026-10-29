//! Windows capture lifecycle, in-process via `windows-capture` (WGC), which
//! links cleanly beside libwebrtc and cpal.
//!
//! `enumerate_screen_sources` lists monitors and windows with 320×200 GDI
//! thumbnails for the in-app picker and caches the handles by index;
//! `start_screen_share(Some(Selection))` looks the pick up. `None` falls back
//! to the native `GraphicsCapturePicker`, so a frontend regression cannot
//! strand the user. Capture runs on one owned thread for both paths (the
//! native picker's item is `!Send`), so `stop_screen_share` has one teardown.

use std::sync::Arc;

use libwebrtc::{
    prelude::{RtcVideoSource, VideoFrame, VideoRotation},
    video_frame::VideoBuffer,
    video_source::{native::NativeVideoSource, VideoResolution},
};
use livekit::{
    options::{TrackPublishOptions, VideoEncoding},
    prelude::*,
    track::{LocalTrack, LocalVideoTrack},
};
use windows_capture::{monitor::Monitor, window::Window};

use crate::{error::Result, state::AppState};

use super::{
    codec::{convert_to_i420, pack_frame_bytes, pick_screenshare_codec, resolve_screenshare_encoding},
    fail_capture, RawSink, ScreenShareEvent, LOCAL_PREVIEW_KEY, PREVIEW_MIN_INTERVAL,
};

// Thumbnail size for the in-app picker (16:10), sent as a base64 PNG data URL.
const THUMB_W: u32 = 320;
const THUMB_H: u32 = 200;

/// The enumeration the in-app picker is rendering, indexed like the proto's
/// `DisplaySource::id` / `WindowSource::id`. The handles are bare
/// `HMONITOR`/`HWND`s and `Send`.
pub struct WindowsPickerCache {
    pub displays: Vec<Monitor>,
    pub windows: Vec<Window>,
}

pub async fn enumerate_screen_sources(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    // Blocking GDI work: off the tokio worker.
    let enumerated = tokio::task::spawn_blocking(enumerate_blocking)
        .await
        .map_err(|e| anyhow::anyhow!("enumerate join: {e}"))??;

    let mut ss = state.screenshare.lock().await;
    ss.windows_picker = Some(WindowsPickerCache {
        displays: enumerated.display_handles,
        windows: enumerated.window_handles,
    });
    Ok(pollis_capture_proto::SourceList {
        displays: enumerated.display_sources,
        windows: enumerated.window_sources,
    })
}

pub async fn cancel_screen_share_picker(state: &Arc<AppState>) -> Result<()> {
    let mut ss = state.screenshare.lock().await;
    ss.windows_picker = None;
    Ok(())
}

/// `with_audio` also captures the machine's audio output minus our own process
/// tree, so the call is never re-published (`super::windows_audio`).
pub async fn start_screen_share(
    state: &Arc<AppState>,
    selection: Option<pollis_capture_proto::Selection>,
    max_framerate: Option<u32>,
    with_audio: bool,
) -> Result<()> {
    use std::sync::atomic::AtomicBool;

    let room = {
        let voice = state.voice.lock().await;
        voice.room.clone()
    };
    let room = room.ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("not in a voice channel — join voice first"))
    })?;

    // Start from a clean slate.
    {
        let has_prev = {
            let ss = state.screenshare.lock().await;
            ss.local_track.is_some() || ss.windows_thread.is_some()
        };
        if has_prev {
            let _ = super::stop::stop_screen_share(state).await;
        }
    }

    // `None`: the native WGC picker (safety net); `Some`: the cached handle.
    let capture_source = match selection {
        None => CaptureSource::NativePicker,
        Some(sel) => {
            let mut ss = state.screenshare.lock().await;
            let cache = ss.windows_picker.take().ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!(
                    "screen-share picker cache is empty — re-open the picker"
                ))
            })?;
            drop(ss);
            match sel {
                pollis_capture_proto::Selection::Display { id, .. } => {
                    let monitor = cache
                        .displays
                        .into_iter()
                        .nth(id as usize)
                        .ok_or_else(|| {
                            crate::error::Error::Other(anyhow::anyhow!(
                                "selected display no longer available — re-open the picker"
                            ))
                        })?;
                    CaptureSource::Monitor(monitor)
                }
                pollis_capture_proto::Selection::Window { id, .. } => {
                    let window = cache
                        .windows
                        .into_iter()
                        .nth(id as usize)
                        .ok_or_else(|| {
                            crate::error::Error::Other(anyhow::anyhow!(
                                "selected window no longer available — re-open the picker"
                            ))
                        })?;
                    CaptureSource::Window(window)
                }
            }
        }
    };

    // Provisional resolution: the first frame carries the real size, which
    // NativeVideoSource tolerates.
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
    // Honour the user's framerate preference, as in start_unix.rs.
    let (max_framerate, max_bitrate) = resolve_screenshare_encoding(max_framerate);
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track.clone()),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: pick_screenshare_codec(),
                video_encoding: Some(VideoEncoding {
                    max_framerate,
                    max_bitrate,
                }),
                ..Default::default()
            },
        )
        .await
    {
        crate::diag!("[screenshare] publish error: {e}");
        return Err(fail_capture(
            state,
            "Could not publish the screen-share to the call. Check your connection and try again.".into(),
        )
        .await);
    }
    crate::diag!("[screenshare] track published");

    let active_flag = std::sync::Arc::new(AtomicBool::new(true));
    let (frames_sink, events_sink) = {
        let ss = state.screenshare.lock().await;
        (ss.frames.clone(), ss.events.clone())
    };
    let preview_tx = state.screenshare_frame_tx.clone();

    let flags = WindowsCaptureFlags {
        source: source.clone(),
        active: std::sync::Arc::clone(&active_flag),
        frames: frames_sink,
        frame_tx: preview_tx,
    };
    // The thread reports the picked size (or cancel/error), then blocks in
    // start() until the frame callback sees the fence.
    let (size_tx, size_rx) = tokio::sync::oneshot::channel::<WgcStart>();
    let events_for_thread = events_sink.clone();
    let capture_thread = std::thread::Builder::new()
        .name("wgc-screenshare".into())
        .spawn(move || run_capture_thread(capture_source, flags, size_tx, events_for_thread))
        .map_err(|e| anyhow::anyhow!("spawn wgc capture thread: {e}"))?;

    let (width, height) = match size_rx.await {
        Ok(WgcStart::Size(w, h)) => (w, h),
        Ok(WgcStart::Cancelled) => {
            // A normal cancel: roll back the publish without a LocalError.
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

    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.windows_thread = Some(capture_thread);
        ss.windows_active = Some(std::sync::Arc::clone(&active_flag));
        ss.windows_picker = None;
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    let _ = events_sink;

    // Audio starts only once video is running and the session is stashed:
    // it must never delay LocalStarted, and an early stop must find a session.
    if with_audio {
        start_windows_audio(state, &room, std::sync::Arc::clone(&active_flag)).await;
    }
    Ok(())
}

/// Start WASAPI process-loopback capture and the task that publishes it. Never
/// fails the share: problems (commonly a pre-20H1 Windows 10) downgrade to
/// video-only via `LocalAudioUnavailable`.
async fn start_windows_audio(
    state: &Arc<AppState>,
    room: &std::sync::Arc<livekit::Room>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use super::audio::{emit, publish_shared_audio_track, SharedAudioResampler,
        SHARED_AUDIO_FRAME_SAMPLES, SHARED_AUDIO_RATE_HZ};
    use super::windows_audio::{run_loopback_capture, AudioBlock};

    // 32 blocks ≈ 300 ms of slack: enough to absorb a publish-side hiccup, too
    // little to hide a stalled consumer.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AudioBlock>(32);
    // std's Result: `crate::error::Result` is a one-generic alias.
    let (start_tx, start_rx) =
        tokio::sync::oneshot::channel::<std::result::Result<(), String>>();
    let active_for_thread = std::sync::Arc::clone(&active);
    let capture_thread = std::thread::Builder::new()
        .name("wasapi-screenshare-audio".into())
        .spawn(move || {
            // COM with callbacks on system threads: MTA. Failing to
            // initialise is an audio-only failure.
            let com = unsafe {
                windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                )
            };
            if com.is_err() {
                let _ = start_tx.send(Err("could not initialise audio COM".into()));
                return;
            }
            // Signals `start_tx` once the stream is up, then blocks until the
            // share ends.
            run_loopback_capture(tx, active_for_thread, start_tx);
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        });
    if capture_thread.is_err() {
        emit(
            state,
            ScreenShareEvent::LocalAudioUnavailable {
                message: "Could not start audio capture. Sharing without sound.".into(),
            },
        )
        .await;
        return;
    }

    // Milliseconds on the happy path; the activation itself gives up after
    // 2 s, so 5 s only guards a wedged audio service.
    match tokio::time::timeout(std::time::Duration::from_millis(5_000), start_rx).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(reason))) => {
            emit(
                state,
                ScreenShareEvent::LocalAudioUnavailable {
                    message: format!("Sharing without sound: {reason}"),
                },
            )
            .await;
            return;
        }
        // Sender dropped before reporting, or the timeout fired.
        Ok(Err(_)) | Err(_) => {
            emit(
                state,
                ScreenShareEvent::LocalAudioUnavailable {
                    message: "Audio capture stopped before it started. Sharing without sound."
                        .into(),
                },
            )
            .await;
            return;
        }
    }

    let Some(source) = publish_shared_audio_track(state, room).await else {
        return;
    };
    let mut resampler = SharedAudioResampler::new(SHARED_AUDIO_RATE_HZ);
    let state_for_task = Arc::clone(state);
    let room_for_task = std::sync::Arc::clone(room);
    tokio::spawn(async move {
        while let Some(block) = rx.recv().await {
            if resampler.src_rate() != block.sample_rate {
                resampler.set_src_rate(block.sample_rate);
            }
            for frame in resampler.push(&block.pcm, block.channels) {
                // No echo canceller needed: the OS excluded our process tree.
                let frame = libwebrtc::prelude::AudioFrame {
                    data: frame.into(),
                    sample_rate: SHARED_AUDIO_RATE_HZ,
                    num_channels: 1,
                    samples_per_channel: SHARED_AUDIO_FRAME_SAMPLES as u32,
                };
                if let Err(e) = source.capture_frame(&frame).await {
                    crate::diag!("[screenshare/audio] capture_frame error: {e:?}");
                }
            }
        }
        crate::diag!("[screenshare/audio] windows publish loop ended");
        // The capture thread hung up while the share still runs: a mid-share
        // audio failure, so retire the track (#1040). On a real stop `active`
        // is false and `stop_screen_share` owns the teardown.
        if active.load(std::sync::atomic::Ordering::Acquire) {
            super::audio::unpublish_shared_audio(&state_for_task, &room_for_task).await;
            drop(source);
            emit(
                &state_for_task,
                ScreenShareEvent::LocalAudioUnavailable {
                    message: "Audio capture stopped. Sharing without sound.".into(),
                },
            )
            .await;
        }
    });
}

/// What the thread captures: the cached in-app pick, or the native picker.
enum CaptureSource {
    NativePicker,
    Monitor(Monitor),
    Window(Window),
}

/// What the WGC thread reports before blocking in `start()`: the size, a user
/// cancel (not an error), or a real failure (surfaced via LocalError).
enum WgcStart {
    Size(u32, u32),
    Cancelled,
    Failed(String),
}

fn run_capture_thread(
    source: CaptureSource,
    flags: WindowsCaptureFlags,
    size_tx: tokio::sync::oneshot::Sender<WgcStart>,
    events_for_thread: Option<Arc<dyn crate::sink::EventSink<ScreenShareEvent>>>,
) {
    use windows_capture::capture::GraphicsCaptureApiHandler;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    // Bgra8: B,G,R,A in memory, as the other platforms feed argb_to_i420.
    macro_rules! settings_for {
        ($item:expr) => {
            Settings::new(
                $item,
                CursorCaptureSettings::WithCursor,
                DrawBorderSettings::Default,
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                flags,
            )
        };
    }

    let started_msg = "Screen capture stopped unexpectedly. Please try sharing again.";

    match source {
        CaptureSource::NativePicker => {
            use windows_capture::graphics_capture_picker::GraphicsCapturePicker;
            let picked = match GraphicsCapturePicker::pick_item() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let _ = size_tx.send(WgcStart::Cancelled);
                    return;
                }
                Err(e) => {
                    crate::diag!("[screenshare] WGC picker error: {e}");
                    let _ = size_tx.send(WgcStart::Failed(
                        "Windows could not open the screen-share picker. Please try again.".into(),
                    ));
                    return;
                }
            };
            let (sw, sh) = match picked.size() {
                Ok(s) => s,
                Err(e) => {
                    crate::diag!("[screenshare] WGC picker size: {e}");
                    let _ = size_tx.send(WgcStart::Failed(
                        "Could not read the selected screen-share source. Please try again."
                            .into(),
                    ));
                    return;
                }
            };
            let width = (sw.max(0) as u32) & !1;
            let height = (sh.max(0) as u32) & !1;
            if width == 0 || height == 0 {
                let _ = size_tx.send(WgcStart::Failed(
                    "The selected screen-share source has an invalid size. Please try again.".into(),
                ));
                return;
            }
            crate::diag!("[screenshare] windows picked {}x{}", width, height);
            let settings = settings_for!(picked);
            let _ = size_tx.send(WgcStart::Size(width, height));
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                crate::diag!("[screenshare] WGC start/stop: {e}");
                if let Some(ev) = &events_for_thread {
                    let _ = ev.send(ScreenShareEvent::LocalError { message: started_msg.into() });
                }
            }
        }
        CaptureSource::Monitor(monitor) => {
            let (width, height) = match (monitor.width(), monitor.height()) {
                (Ok(w), Ok(h)) => (w & !1, h & !1),
                _ => {
                    let _ = size_tx.send(WgcStart::Failed(
                        "Could not read the selected display's size. Please try again.".into(),
                    ));
                    return;
                }
            };
            if width == 0 || height == 0 {
                let _ = size_tx.send(WgcStart::Failed(
                    "The selected display has an invalid size. Please try again.".into(),
                ));
                return;
            }
            crate::diag!("[screenshare] in-app picker (display) {}x{}", width, height);
            let settings = settings_for!(monitor);
            let _ = size_tx.send(WgcStart::Size(width, height));
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                crate::diag!("[screenshare] WGC start/stop: {e}");
                if let Some(ev) = &events_for_thread {
                    let _ = ev.send(ScreenShareEvent::LocalError { message: started_msg.into() });
                }
            }
        }
        CaptureSource::Window(window) => {
            let (width, height) = match (window.width(), window.height()) {
                (Ok(w), Ok(h)) if w > 0 && h > 0 => ((w as u32) & !1, (h as u32) & !1),
                _ => {
                    let _ = size_tx.send(WgcStart::Failed(
                        "Could not read the selected window's size. The window may have closed.".into(),
                    ));
                    return;
                }
            };
            if width == 0 || height == 0 {
                let _ = size_tx.send(WgcStart::Failed(
                    "The selected window has an invalid size. Please try again.".into(),
                ));
                return;
            }
            crate::diag!("[screenshare] in-app picker (window) {}x{}", width, height);
            let settings = settings_for!(window);
            let _ = size_tx.send(WgcStart::Size(width, height));
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                crate::diag!("[screenshare] WGC start/stop: {e}");
                if let Some(ev) = &events_for_thread {
                    let _ = ev.send(ScreenShareEvent::LocalError { message: started_msg.into() });
                }
            }
        }
    }
}

struct EnumeratedSources {
    display_sources: Vec<pollis_capture_proto::DisplaySource>,
    display_handles: Vec<Monitor>,
    window_sources: Vec<pollis_capture_proto::WindowSource>,
    window_handles: Vec<Window>,
}

fn enumerate_blocking() -> Result<EnumeratedSources> {
    let monitors = Monitor::enumerate()
        .map_err(|e| anyhow::anyhow!("enumerate monitors: {e}"))?;
    let mut display_sources = Vec::with_capacity(monitors.len());
    let mut display_handles = Vec::with_capacity(monitors.len());
    for (idx, monitor) in monitors.into_iter().enumerate() {
        let width = monitor.width().unwrap_or(0);
        let height = monitor.height().unwrap_or(0);
        // EDID name, else the device path (`\\.\DISPLAY1`), else a placeholder.
        let name = monitor
            .name()
            .or_else(|_| monitor.device_name())
            .unwrap_or_else(|_| format!("Display {}", idx + 1));
        let thumbnail = capture_monitor_thumbnail(&monitor);
        display_sources.push(pollis_capture_proto::DisplaySource {
            id: idx as u32,
            width,
            height,
            name,
            thumbnail_data_url: thumbnail,
        });
        display_handles.push(monitor);
    }

    let windows = Window::enumerate()
        .map_err(|e| anyhow::anyhow!("enumerate windows: {e}"))?;
    let mut window_sources = Vec::with_capacity(windows.len());
    let mut window_handles = Vec::with_capacity(windows.len());
    for window in windows.into_iter() {
        let title = window.title().unwrap_or_default();
        // Needs PROCESS_VM_READ, which elevated processes deny; empty is fine.
        let app_name = window.process_name().unwrap_or_default();
        // Skip cloaked, tiny and unlabelled windows (UWP frame hosts, tray
        // apps), as Slack and Discord do.
        if !is_window_user_facing(&window, &title, &app_name) {
            continue;
        }
        let width = window.width().ok().map(|w| w.max(0) as u32).unwrap_or(0);
        let height = window.height().ok().map(|h| h.max(0) as u32).unwrap_or(0);
        let thumbnail = capture_window_thumbnail(&window);
        // Assigned after the filter so it matches `window_handles`, the index
        // `Selection::Window` passes back.
        let idx = window_handles.len() as u32;
        window_sources.push(pollis_capture_proto::WindowSource {
            id: idx,
            width,
            height,
            title,
            app_name,
            bundle_id: String::new(),
            thumbnail_data_url: thumbnail,
        });
        window_handles.push(window);
    }

    Ok(EnumeratedSources {
        display_sources,
        display_handles,
        window_sources,
        window_handles,
    })
}

// `Window::enumerate()` still leaves cloaked UWP frame hosts (visible per
// `IsWindowVisible`, invisible to the user), tray apps' near-zero-size
// message windows, and untitled helper windows. 64×64 matches the macOS
// helper's floor.
const MIN_WINDOW_DIM: i32 = 64;

fn is_window_user_facing(window: &Window, title: &str, app_name: &str) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

    // Any non-zero DWM cloak value hides it. If the call fails, fall through:
    // better to show a tile than hide a real window.
    let hwnd = HWND(window.as_raw_hwnd());
    let mut cloaked: u32 = 0;
    let cloak_ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    if cloak_ok.is_ok() && cloaked != 0 {
        return false;
    }

    let (w, h) = match (window.width(), window.height()) {
        (Ok(w), Ok(h)) => (w, h),
        _ => return false,
    };
    if w < MIN_WINDOW_DIM || h < MIN_WINDOW_DIM {
        return false;
    }

    if title.is_empty() && app_name.is_empty() {
        return false;
    }

    true
}

fn capture_monitor_thumbnail(monitor: &Monitor) -> Option<String> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, GetDC, GetMonitorInfoW, HALFTONE, HMONITOR,
        MONITORINFO, MONITORINFOEXW, SRCCOPY, SelectObject, SetStretchBltMode, StretchBlt,
    };

    unsafe {
        let hmonitor = HMONITOR(monitor.as_raw_hmonitor());
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                rcMonitor: RECT::default(),
                rcWork: RECT::default(),
                dwFlags: 0,
            },
            szDevice: [0; 32],
        };
        if !GetMonitorInfoW(hmonitor, (&raw mut info).cast()).as_bool() {
            return None;
        }
        let src_rect = info.monitorInfo.rcMonitor;
        let src_w = src_rect.right - src_rect.left;
        let src_h = src_rect.bottom - src_rect.top;
        if src_w <= 0 || src_h <= 0 {
            return None;
        }

        // GetDC(None) is the desktop DC.
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return None;
        }
        let scope = ScopedScreenDc(screen_dc);

        let mem_dc = CreateCompatibleDC(Some(scope.0));
        if mem_dc.is_invalid() {
            return None;
        }
        let mem_scope = ScopedDc(mem_dc);

        let bitmap = CreateCompatibleBitmap(scope.0, THUMB_W as i32, THUMB_H as i32);
        if bitmap.is_invalid() {
            return None;
        }
        let _bm_scope = ScopedBitmap(bitmap);
        let old = SelectObject(mem_scope.0, bitmap.into());

        SetStretchBltMode(mem_scope.0, HALFTONE);

        let ok = StretchBlt(
            mem_scope.0,
            0,
            0,
            THUMB_W as i32,
            THUMB_H as i32,
            Some(scope.0),
            src_rect.left,
            src_rect.top,
            src_w,
            src_h,
            SRCCOPY,
        )
        .as_bool();
        if !ok {
            SelectObject(mem_scope.0, old);
            return None;
        }

        let bgra = read_dib_bgra(mem_scope.0, bitmap);
        SelectObject(mem_scope.0, old);
        let bgra = bgra?;
        encode_thumbnail_data_url(&bgra)
    }
}

fn capture_window_thumbnail(window: &Window) -> Option<String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, GetDC, HALFTONE, SRCCOPY, SelectObject,
        SetStretchBltMode, StretchBlt,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    unsafe {
        let hwnd = HWND(window.as_raw_hwnd());

        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }
        let src_w = rect.right - rect.left;
        let src_h = rect.bottom - rect.top;
        if src_w <= 0 || src_h <= 0 {
            return None;
        }

        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return None;
        }
        let screen_scope = ScopedScreenDc(screen_dc);

        // Render the window at full size via PrintWindow.
        let src_mem_dc = CreateCompatibleDC(Some(screen_scope.0));
        if src_mem_dc.is_invalid() {
            return None;
        }
        let src_scope = ScopedDc(src_mem_dc);
        let src_bitmap = CreateCompatibleBitmap(screen_scope.0, src_w, src_h);
        if src_bitmap.is_invalid() {
            return None;
        }
        let _src_bm_scope = ScopedBitmap(src_bitmap);
        let src_old = SelectObject(src_scope.0, src_bitmap.into());

        // PW_RENDERFULLCONTENT (0x2): needed for Chromium and
        // DirectComposition windows (Win10 1903+).
        let ok = PrintWindow(hwnd, src_scope.0, PRINT_WINDOW_FLAGS(0x00000002));
        if !ok.as_bool() {
            let retry = PrintWindow(hwnd, src_scope.0, PRINT_WINDOW_FLAGS(0));
            if !retry.as_bool() {
                SelectObject(src_scope.0, src_old);
                return None;
            }
        }

        let dst_mem_dc = CreateCompatibleDC(Some(screen_scope.0));
        if dst_mem_dc.is_invalid() {
            SelectObject(src_scope.0, src_old);
            return None;
        }
        let dst_scope = ScopedDc(dst_mem_dc);
        let dst_bitmap = CreateCompatibleBitmap(screen_scope.0, THUMB_W as i32, THUMB_H as i32);
        if dst_bitmap.is_invalid() {
            SelectObject(src_scope.0, src_old);
            return None;
        }
        let _dst_bm_scope = ScopedBitmap(dst_bitmap);
        let dst_old = SelectObject(dst_scope.0, dst_bitmap.into());

        SetStretchBltMode(dst_scope.0, HALFTONE);

        let ok = StretchBlt(
            dst_scope.0,
            0,
            0,
            THUMB_W as i32,
            THUMB_H as i32,
            Some(src_scope.0),
            0,
            0,
            src_w,
            src_h,
            SRCCOPY,
        )
        .as_bool();
        if !ok {
            SelectObject(src_scope.0, src_old);
            SelectObject(dst_scope.0, dst_old);
            return None;
        }

        let bgra = read_dib_bgra(dst_scope.0, dst_bitmap);
        SelectObject(src_scope.0, src_old);
        SelectObject(dst_scope.0, dst_old);
        let bgra = bgra?;
        encode_thumbnail_data_url(&bgra)
    }
}

/// Read the THUMB_W × THUMB_H bitmap selected into `dc` as top-down BGRA8.
unsafe fn read_dib_bgra(
    dc: windows::Win32::Graphics::Gdi::HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{
        GetDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };

    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: THUMB_W as i32,
        // Negative: a top-down DIB.
        biHeight: -(THUMB_H as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let mut info = BITMAPINFO {
        bmiHeader: header,
        bmiColors: [Default::default(); 1],
    };
    let mut buf = vec![0u8; (THUMB_W * THUMB_H * 4) as usize];
    let lines = GetDIBits(
        dc,
        bitmap,
        0,
        THUMB_H,
        Some(buf.as_mut_ptr().cast()),
        &mut info,
        DIB_RGB_COLORS,
    );
    if lines == 0 {
        return None;
    }
    Some(buf)
}

fn encode_thumbnail_data_url(bgra: &[u8]) -> Option<String> {
    use base64::Engine;
    use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

    let expected = (THUMB_W * THUMB_H * 4) as usize;
    if bgra.len() != expected {
        return None;
    }
    let mut rgb = Vec::with_capacity((THUMB_W * THUMB_H * 3) as usize);
    for chunk in bgra.chunks_exact(4) {
        rgb.push(chunk[2]);
        rgb.push(chunk[1]);
        rgb.push(chunk[0]);
    }

    let mut png_bytes = Vec::with_capacity(16 * 1024);
    let encoder = PngEncoder::new(&mut png_bytes);
    encoder
        .write_image(&rgb, THUMB_W, THUMB_H, ExtendedColorType::Rgb8)
        .ok()?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Some(format!("data:image/png;base64,{encoded}"))
}

// RAII guards: GDI handles leak unless released on every path.

struct ScopedScreenDc(windows::Win32::Graphics::Gdi::HDC);
impl Drop for ScopedScreenDc {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::Graphics::Gdi::ReleaseDC(None, self.0);
        }
    }
}

struct ScopedDc(windows::Win32::Graphics::Gdi::HDC);
impl Drop for ScopedDc {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteDC(self.0);
        }
    }
}

struct ScopedBitmap(windows::Win32::Graphics::Gdi::HBITMAP);
impl Drop for ScopedBitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.0.into());
        }
    }
}

struct WindowsCaptureFlags {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
    frame_tx: tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>,
}

struct WindowsCaptureHandler {
    source: NativeVideoSource,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    frames: Option<Arc<dyn RawSink>>,
    frame_tx: tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>,
    // WGC serializes the callback (&mut self), so no Mutex.
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
            frame_tx: ctx.flags.frame_tx,
            last_preview: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut windows_capture::frame::Frame<'_>,
        capture_control: windows_capture::graphics_capture_api::InternalCaptureControl,
    ) -> std::result::Result<(), Self::Error> {
        if !self.active.load(std::sync::atomic::Ordering::Acquire) {
            capture_control.stop();
            return Ok(());
        }
        let mut buffer = frame.buffer().map_err(|e| -> Self::Error { Box::new(e) })?;
        let width = buffer.width();
        let height = buffer.height();
        // The GPU-aligned stride (>= width*4), which argb_to_i420 takes as is.
        let stride = buffer.row_pitch();
        let bgra = buffer.as_raw_buffer();
        let timestamp_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        // The WS preview gets every frame; only the legacy RawSink keeps the
        // PREVIEW_MIN_INTERVAL gate (as in start_unix.rs).
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
        push_frame_windows(
            &self.source,
            width,
            height,
            stride,
            timestamp_us,
            bgra,
            preview,
            Some(&self.frame_tx),
        );
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
    preview_ws: Option<&tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>>,
) {
    // VP8 and I420 require even dimensions.
    let w = (width & !1) as i32;
    let h = (height & !1) as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    let buffer = convert_to_i420(w, h, stride, bgra);
    let (out_w, out_h) = (buffer.width(), buffer.height());
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        frame_metadata: None,
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
