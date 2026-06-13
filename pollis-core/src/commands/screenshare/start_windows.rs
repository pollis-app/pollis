//! Windows capture lifecycle. In-process via the `windows-capture` crate
//! (Windows.Graphics.Capture). Like macOS, no subprocess is needed — WGC
//! is a clean in-proc linkage and doesn't fight libwebrtc/cpal/Tauri the
//! way Linux's libpipewire does.
//!
//! Source selection (mirrors macOS, since spike/tauri-revival):
//!   `enumerate_screen_sources` runs `Monitor::enumerate()` +
//!   `Window::enumerate()`, captures a 320×200 GDI thumbnail per source
//!   (BitBlt for monitors, PrintWindow for windows), and returns a
//!   `SourceList` the frontend's in-app picker renders directly. The raw
//!   `Monitor` / `Window` handles are cached in `ScreenShareState::windows_picker`,
//!   keyed by 0-based index. `start_screen_share(Some(Selection))` looks the
//!   handle up and feeds it straight into `Settings`. `Selection::None` is a
//!   safety-net path that falls back to the native `GraphicsCapturePicker`,
//!   preserved so a frontend regression can't strand the user.
//!
//! Capture flow:
//!   1. Resolve the capture source — cached Monitor/Window for the
//!      in-app picker path, or `GraphicsCapturePicker::pick_item()` for
//!      the native fallback.
//!   2. Create the LiveKit NativeVideoSource + LocalVideoTrack, publish
//!      to the current voice room as Screenshare/VP8.
//!   3. Run the blocking `WindowsCaptureHandler::start(settings)` on a
//!      dedicated owned thread. The frame callback converts every BGRA8
//!      WGC frame to I420 inline (off the tokio runtime — WGC pumps on
//!      its own worker thread).
//!   4. Stash the JoinHandle + per-session fence in state so stop is
//!      synchronous + ordered with the track unpublish.
//!
//! We use one dedicated thread (not `start_free_threaded`) because the
//! native-picker fallback's `PickedGraphicsCaptureItem` is `!Send`. With
//! the in-app picker path `Monitor`/`Window` ARE `Send`, but keeping the
//! same thread structure across both paths means one set of teardown
//! semantics in `stop_screen_share`.

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

// ── Thumbnail size for the in-app picker ──────────────────────────────────
//
// Matches the size the Electron path requests from `desktopCapturer.getSources`
// — 320×200, 16:10 — so the same `ScreenSharePicker` grid renders consistently
// across runtimes. The bytes go out as a base64-encoded PNG data URL.
const THUMB_W: u32 = 320;
const THUMB_H: u32 = 200;

// ── Picker cache ──────────────────────────────────────────────────────────

/// Cached enumeration result the frontend's in-app picker is rendering.
/// `Monitor`/`Window` are `Send` and hold a bare `HMONITOR`/`HWND`, so
/// stashing them is cheap and they cross threads safely when handed to
/// the capture thread. Indices match the `DisplaySource::id` /
/// `WindowSource::id` values returned in the proto.
pub struct WindowsPickerCache {
    pub displays: Vec<Monitor>,
    pub windows: Vec<Window>,
}

// ── Commands ──────────────────────────────────────────────────────────────

pub async fn enumerate_screen_sources(
    state: &Arc<AppState>,
) -> Result<pollis_capture_proto::SourceList> {
    // EnumDisplayMonitors / EnumChildWindows + the GDI thumbnail capture
    // are blocking GDI work — keep them off the tokio worker.
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

pub async fn start_screen_share(
    state: &Arc<AppState>,
    selection: Option<pollis_capture_proto::Selection>,
    max_framerate: Option<u32>,
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

    // Resolve the capture source. `Selection::None` -> native WGC picker
    // (rare safety-net path); `Some` -> cached handle from the in-app picker.
    let capture_source = match selection {
        None => CaptureSource::NativePicker,
        Some(sel) => {
            let mut ss = state.screenshare.lock().await;
            let cache = ss.windows_picker.take().ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!(
                    "screen-share picker cache is empty — re-open the picker"
                ))
            })?;
            // Drop the lock before doing anything else.
            drop(ss);
            match sel {
                pollis_capture_proto::Selection::Display { id } => {
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
                pollis_capture_proto::Selection::Window { id } => {
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
    // See start_unix.rs: honour the user's Screen Share framerate preference
    // (default 30) so the ceiling tracks the source.
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
    let preview_tx = state.screenshare_frame_tx.clone();

    let flags = WindowsCaptureFlags {
        source: source.clone(),
        active: std::sync::Arc::clone(&active_flag),
        frames: frames_sink,
        frame_tx: preview_tx,
    };
    // The thread reports the picked size (or a picker cancel/error) back
    // over a oneshot, then blocks in start() until the frame callback sees
    // the fence flipped.
    let (size_tx, size_rx) = tokio::sync::oneshot::channel::<WgcStart>();
    let events_for_thread = events_sink.clone();
    let capture_thread = std::thread::Builder::new()
        .name("wgc-screenshare".into())
        .spawn(move || run_capture_thread(capture_source, flags, size_tx, events_for_thread))
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

    // 3. Stash for stop_screen_share + announce.
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_source = Some(source);
        ss.local_track = Some(track);
        ss.windows_thread = Some(capture_thread);
        ss.windows_active = Some(std::sync::Arc::clone(&active_flag));
        // Successful start consumes the picker cache.
        ss.windows_picker = None;
        if let Some(ev) = &ss.events {
            let _ = ev.send(ScreenShareEvent::LocalStarted { width, height });
        }
    }
    let _ = events_sink;
    Ok(())
}

// ── Capture-thread driver ─────────────────────────────────────────────────

/// What the thread is going to capture: cached in-app picker selection,
/// or the legacy native WGC picker (frontend `selection: None` fallback).
enum CaptureSource {
    NativePicker,
    Monitor(Monitor),
    Window(Window),
}

/// Outcome the dedicated WGC thread reports before it blocks in
/// `start()`: the negotiated size, a clean user cancel (not surfaced as
/// an error the UI must react to), or a genuine capture failure
/// (surfaced via LocalError).
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

    // Common settings constructor — keeps every arm's call site one line.
    // Bgra8 so the bytes are B,G,R,A in memory — identical to the
    // macOS/Linux paths feeding libwebrtc argb_to_i420, no swizzle.
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
                    eprintln!("[screenshare] WGC picker error: {e}");
                    let _ = size_tx.send(WgcStart::Failed(
                        "Windows could not open the screen-share picker. Please try again.".into(),
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
            let width = (sw.max(0) as u32) & !1;
            let height = (sh.max(0) as u32) & !1;
            if width == 0 || height == 0 {
                let _ = size_tx.send(WgcStart::Failed(
                    "The selected screen-share source has an invalid size. Please try again.".into(),
                ));
                return;
            }
            eprintln!("[screenshare] windows picked {}x{}", width, height);
            let settings = settings_for!(picked);
            let _ = size_tx.send(WgcStart::Size(width, height));
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                eprintln!("[screenshare] WGC start/stop: {e}");
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
            eprintln!("[screenshare] in-app picker (display) {}x{}", width, height);
            let settings = settings_for!(monitor);
            let _ = size_tx.send(WgcStart::Size(width, height));
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                eprintln!("[screenshare] WGC start/stop: {e}");
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
            eprintln!("[screenshare] in-app picker (window) {}x{}", width, height);
            let settings = settings_for!(window);
            let _ = size_tx.send(WgcStart::Size(width, height));
            if let Err(e) = WindowsCaptureHandler::start(settings) {
                eprintln!("[screenshare] WGC start/stop: {e}");
                if let Some(ev) = &events_for_thread {
                    let _ = ev.send(ScreenShareEvent::LocalError { message: started_msg.into() });
                }
            }
        }
    }
}

// ── Enumeration (blocking GDI work) ───────────────────────────────────────

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
        // Prefer the friendly EDID name; fall back to the device path
        // (`\\.\DISPLAY1`) and then a generic placeholder so the tile is
        // never label-less.
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
        // process_name needs PROCESS_VM_READ — fails for elevated processes
        // and tightly-locked-down system processes. Don't fail enumeration
        // over it; empty string is fine.
        let app_name = window.process_name().unwrap_or_default();
        // Skip headless/cloaked/microscopic windows — UWP frame hosts
        // (ApplicationFrameHost), suspended store apps, audio-console-style
        // persistent invisible top-level windows, etc. Same gate
        // Slack/Discord apply.
        if !is_window_user_facing(&window, &title, &app_name) {
            continue;
        }
        let width = window.width().ok().map(|w| w.max(0) as u32).unwrap_or(0);
        let height = window.height().ok().map(|h| h.max(0) as u32).unwrap_or(0);
        let thumbnail = capture_window_thumbnail(&window);
        // Index is assigned AFTER the filter so it matches `window_handles`
        // exactly — that's the index `Selection::Window` passes back.
        let idx = window_handles.len() as u32;
        window_sources.push(pollis_capture_proto::WindowSource {
            id: idx,
            width,
            height,
            title,
            app_name,
            // bundle_id is macOS-only; Windows has no analog.
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

// ── User-facing window filter ─────────────────────────────────────────────
//
// `Window::enumerate()` only filters out the obvious junk: invisible windows,
// tool windows, child windows, and our own process. That still leaves three
// big classes of pickerable-but-useless surfaces:
//
//   * **UWP frame hosts** (`ApplicationFrameHost.exe`) for store apps that
//     are running but cloaked — minimized to the start screen, suspended in
//     the background. They report `IsWindowVisible() == TRUE` even when the
//     user has no on-screen surface for them. The DWM cloak attribute is
//     the canonical check; Slack/Discord/Teams all key off it.
//   * **Tray-resident utility apps** like Realtek Audio Console — Win32 apps
//     that keep a "visible" top-level window of effectively zero size to
//     receive shell messages, even when the user only ever sees the system
//     tray icon.
//   * **Pure helper windows** with no title and no resolvable process name
//     (system shell overlays that survived the IsWindowVisible cull).
//
// 64×64 is the same floor the macOS helper uses. Real picker targets are
// always at least a few hundred pixels in either dimension.
const MIN_WINDOW_DIM: i32 = 64;

fn is_window_user_facing(window: &Window, title: &str, app_name: &str) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

    // 1. Filter cloaked UWP / shell-cloaked windows. Treat any non-zero
    //    cloak value as "don't show" (DWM_CLOAKED_APP / DWM_CLOAKED_SHELL /
    //    DWM_CLOAKED_INHERITED are all reasons the user can't see it).
    //    If the API call fails (older Windows / restricted process), we
    //    fall through to the size + label checks — better to show a tile
    //    than wrongly hide a real window.
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

    // 2. Filter sub-64px windows. Anything smaller is almost always a
    //    helper / message-loop sink.
    let (w, h) = match (window.width(), window.height()) {
        (Ok(w), Ok(h)) => (w, h),
        _ => return false,
    };
    if w < MIN_WINDOW_DIM || h < MIN_WINDOW_DIM {
        return false;
    }

    // 3. Need *something* to label the tile with.
    if title.is_empty() && app_name.is_empty() {
        return false;
    }

    true
}

// ── Thumbnail capture ─────────────────────────────────────────────────────

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

        // GetDC(None) yields the desktop (root window) DC; ReleaseDC accepts
        // None symmetrically. windows-rs encodes the nullable HWND param as
        // Option<HWND> in 0.62+.
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

        // 1. Render the window full-resolution into a memory DC via PrintWindow.
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

        // PW_RENDERFULLCONTENT (0x00000002) — required for Chromium-based
        // and DirectComposition-based windows. Win10 1903+.
        let ok = PrintWindow(hwnd, src_scope.0, PRINT_WINDOW_FLAGS(0x00000002));
        if !ok.as_bool() {
            // Fallback without the flag — better than no thumbnail.
            let retry = PrintWindow(hwnd, src_scope.0, PRINT_WINDOW_FLAGS(0));
            if !retry.as_bool() {
                SelectObject(src_scope.0, src_old);
                return None;
            }
        }

        // 2. StretchBlt the native-size render down to THUMB_W × THUMB_H.
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

/// Read the THUMB_W × THUMB_H bitmap selected into `dc` as a flat BGRA8
/// vector (top-down).
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
        // Negative for top-down DIB so we don't have to flip rows later.
        biHeight: -(THUMB_H as i32),
        biPlanes: 1,
        biBitCount: 32,
        // BI_RGB is a typed wrapper around 0u32; biCompression is a raw u32.
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
    // BGRA -> RGB (drop alpha; thumbnails don't need transparency).
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

// ── GDI scope guards ──────────────────────────────────────────────────────
//
// GDI handles leak if not released on every path (including error paths).
// These tiny RAII wrappers keep `?` and early returns safe.

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

// ── Capture handler + frame push ──────────────────────────────────────────

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
            frame_tx: ctx.flags.frame_tx,
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
        // WS preview rides every frame: the PoC showed the WS+WebGL path is
        // cheap, so there's no reason to throttle the sharer's own tile. The
        // legacy RawSink (unused by the Tauri WS frontend) keeps its
        // PREVIEW_MIN_INTERVAL gate to bound IPC cost if anything still
        // listens on it. Mirrors start_unix.rs.
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
