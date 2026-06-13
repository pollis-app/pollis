use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use pollis_capture_proto::{
    encode_error, encode_format, encode_frame_header, encode_sources, read_msg, CaptureMsg,
    DisplaySource, Selection, SourceList, WindowSource,
};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(name = "pollis-capture-macos", version)]
struct Args {
    /// Path of the Unix socket the parent (Pollis main) is listening on.
    /// Closing this socket is the parent's signal to make us exit.
    #[arg(long)]
    socket: String,
}

/// Channel payload from SCK's dispatch queue (sync world) into the tokio
/// task (async world) that owns the socket. We pre-serialize on the SCK
/// queue so the async side just does socket writes — keeps the protocol
/// bytes defined exactly once (`pollis-capture-proto`) and the hot path
/// allocation-bounded.
enum Wire {
    /// A fully-encoded protocol message ready to write verbatim.
    Bytes(Vec<u8>),
    /// A frame: encoded header + the BGRx payload, written as two
    /// `write_all`s so the (large) payload isn't copied again.
    Frame { header: Vec<u8>, bgrx: Vec<u8> },
}

pub fn run() -> Result<()> {
    let args = Args::parse();

    // Promote to an accessory Cocoa app at runtime. Display capture
    // works without this (Mach services), but per-window SCStream
    // asserts in CoreGraphics with `CGS_REQUIRE_INIT` when the
    // process has no window-server connection. NSApplication +
    // accessory activation policy connects us to the window server
    // without showing a Dock icon or menu bar. No Info.plist needed.
    install_accessory_app();

    // tokio runtime lives on a worker thread because `NSApp.run()`
    // owns the main thread forever; when the worker decides to exit
    // (parent socket closed / capture done) it calls process::exit
    // and the kernel reaps both threads.
    let socket = args.socket;
    std::thread::Builder::new()
        .name("pollis-capture-tokio".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[capture-mac] tokio runtime: {e}");
                    std::process::exit(1);
                }
            };
            let result = rt.block_on(run_async(&socket));
            if let Err(e) = &result {
                eprintln!("[capture-mac] {e}");
            }
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        })?;

    run_main_loop();
    // run_main_loop never returns; the worker thread always
    // process::exit's first. Unreachable but required for the type.
    Ok(())
}

fn install_accessory_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new()
        .expect("install_accessory_app must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    #[allow(deprecated)]
    app.finishLaunching();
}

fn run_main_loop() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let mtm = MainThreadMarker::new().expect("run_main_loop must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Blocking call into Cocoa's main run loop. Only returns on
    // `[NSApp terminate:]`, which we never invoke — the tokio worker
    // exits via `process::exit` instead.
    app.run();
}

async fn run_async(socket_path: &str) -> Result<()> {
    eprintln!("[capture-mac] connecting to parent socket {socket_path}");
    let sock = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to {socket_path}"))?;
    let (read_half, mut write_half) = sock.into_split();
    eprintln!("[capture-mac] connected");

    // macOS has no PR_SET_PDEATHSIG. Poll getppid(): if the parent dies
    // it reparents to launchd (ppid becomes 1) — exit so we don't leak a
    // live SCStream. Cheap, 1 s cadence.
    spawn_parent_death_watch();

    // ── Phase 1: enumerate + send the source list ───────────────────────
    //
    // SCShareableContent is the API Slack/Discord/Zoom/OBS use to
    // enumerate displays + windows for their in-app pickers. We send
    // the result to the parent verbatim; the parent renders the picker
    // UI in the webview and sends back a Select.
    eprintln!("[capture-mac] enumerating shareable content");
    let (list, content_cache) = match enumerate_sources() {
        Ok(pair) => pair,
        Err(e) => {
            // Permission denial / no displays / etc — surface as a
            // structured protocol Error. Send the raw inner message
            // (no prefix); the parent and frontend's
            // friendly-error mapping rely on substring matching
            // ("permission" / "declined") and a prefix would break it.
            let msg = format!("{e}");
            let _ = write_half.write_all(&encode_error(&msg)).await;
            return Err(anyhow!(msg));
        }
    };
    eprintln!(
        "[capture-mac] enumerated {} displays, {} windows",
        list.displays.len(),
        list.windows.len()
    );
    write_half
        .write_all(&encode_sources(&list))
        .await
        .context("send Sources")?;

    // ── Phase 2: wait for the user's pick (Select) ──────────────────────
    //
    // Parent reads Sources, renders the picker, user clicks, parent
    // sends Select. Generous timeout to cover human-pace decision.
    let mut reader = BufReader::with_capacity(4096, read_half);
    let select = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        read_msg(&mut reader),
    )
    .await
    .map_err(|_| anyhow!("timed out waiting for screen-share selection"))?;
    let selection = match select {
        Ok(Some(CaptureMsg::Select(sel))) => sel,
        Ok(Some(other)) => {
            return Err(anyhow!(
                "unexpected message before Select: {other:?}"
            ));
        }
        Ok(None) => {
            // Parent closed the socket — user cancelled the picker. Not
            // an error.
            eprintln!("[capture-mac] parent closed socket before Select — exiting");
            return Ok(());
        }
        Err(e) => return Err(anyhow!("read Select: {e}")),
    };
    eprintln!("[capture-mac] received selection: {selection:?}");

    // ── Phase 3: build SCContentFilter + SCStream, stream frames ────────
    let (tx, mut rx) = mpsc::channel::<Wire>(2);
    let stop = Arc::new(AtomicBool::new(false));

    // Start SCK on a dedicated blocking context: filter construction and
    // SCStream::start_capture are Swift FFI. Errors before the first
    // frame are sent as 0xFF and end the run.
    let stop_for_cap = Arc::clone(&stop);
    let tx_for_cap = tx.clone();
    let _cap_handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = start_capture(content_cache, selection, tx_for_cap.clone(), stop_for_cap) {
            eprintln!("[capture-mac] capture error: {e}");
            let _ =
                tx_for_cap.blocking_send(Wire::Bytes(encode_error(&format!("capture: {e}"))));
        }
    });

    // Drain channel → socket. On any write error (EPIPE, parent gone)
    // stop and exit; the SCK side observes `stop` on its next frame.
    let mut sock = write_half;
    while let Some(item) = rx.recv().await {
        match item {
            Wire::Bytes(b) => {
                if let Err(e) = sock.write_all(&b).await {
                    eprintln!("[capture-mac] socket write error: {e} — exiting");
                    break;
                }
            }
            Wire::Frame { header, bgrx } => {
                if let Err(e) = sock.write_all(&header).await {
                    eprintln!("[capture-mac] socket write error: {e} — exiting");
                    break;
                }
                if let Err(e) = sock.write_all(&bgrx).await {
                    eprintln!("[capture-mac] socket write error: {e} — exiting");
                    break;
                }
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    Ok(())
}

/// Poll getppid(); exit if reparented to launchd (parent died).
fn spawn_parent_death_watch() {
    let original_ppid = unsafe { libc::getppid() };
    std::thread::Builder::new()
        .name("pollis-capture-ppid".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let ppid = unsafe { libc::getppid() };
            if ppid != original_ppid || ppid == 1 {
                eprintln!("[capture-mac] parent died (ppid {ppid}) — exiting");
                std::process::exit(0);
            }
        })
        .ok();
}

/// Cached enumeration result kept alive across the Select wait so the
/// `SCDisplay` / `SCWindow` handles needed to build the filter are still
/// valid (the crate types hold retained Apple pointers).
struct ContentCache {
    displays: Vec<screencapturekit::shareable_content::SCDisplay>,
    windows: Vec<screencapturekit::shareable_content::SCWindow>,
}

/// Run `SCShareableContent::get()` and project the result into our
/// transport-layer `SourceList`. Returns the raw SCK types alongside so
/// `start_capture` can build the filter without a second enumeration.
fn enumerate_sources() -> Result<(SourceList, ContentCache)> {
    use screencapturekit::shareable_content::SCShareableContent;

    // Mirrors what Slack/Discord do: skip off-screen + desktop layer.
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .map_err(|e| anyhow!("SCShareableContent: {e}"))?;

    let raw_displays = content.displays();
    let displays: Vec<DisplaySource> = raw_displays
        .iter()
        .enumerate()
        .map(|(i, d)| DisplaySource {
            id: d.display_id(),
            width: d.width(),
            height: d.height(),
            // SCK doesn't expose a per-display human name; "Display N"
            // is what every other macOS app shows.
            name: format!("Display {}", i + 1),
            // Helper doesn't render thumbnails; picker shows the icon.
            thumbnail_data_url: None,
        })
        .collect();

    let raw_windows = content.windows();
    let windows: Vec<WindowSource> = raw_windows
        .iter()
        // Window layer 0 = normal app window. Layers >= 1 are floating
        // panels, the Dock, menu-bar status items, system overlays —
        // none of those are things a user wants to "share". Negative
        // layers (e.g. desktop background) also unhelpful. Slack /
        // Discord filter identically.
        .filter(|w| w.window_layer() == 0)
        // `is_on_screen` excludes minimized + off-screen, but Apple
        // also flags some agent-process windows as on-screen even
        // when they're 1×1 pixel invisibles. The title + size
        // filter below catches those.
        .filter(|w| w.is_on_screen())
        .filter_map(|w| {
            let app = w.owning_application();
            let app_name = app
                .as_ref()
                .map(|a| a.application_name())
                .unwrap_or_default();
            let bundle_id = app
                .as_ref()
                .map(|a| a.bundle_identifier())
                .unwrap_or_default();
            let title = w.title().unwrap_or_default();
            let frame = w.frame();
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let w_px = frame.width as u32;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let h_px = frame.height as u32;

            // A real user-visible window has both a title and a
            // non-trivial size. Anything missing either is almost
            // always an invisible helper window (menu-bar agent,
            // notification listener, IME panel, Docker daemon, etc.).
            // The 64-pixel floor is generous — most real picker
            // targets are at least a few hundred px.
            if title.is_empty() {
                return None;
            }
            if w_px < 64 || h_px < 64 {
                return None;
            }
            // Drop our own windows from the list — no value to share
            // Pollis to itself, and it can produce odd feedback loops.
            if bundle_id == "xyz.pollis.desktop" || app_name == "Pollis" {
                return None;
            }

            Some(WindowSource {
                id: w.window_id(),
                width: w_px,
                height: h_px,
                title,
                app_name,
                bundle_id,
                thumbnail_data_url: None,
            })
        })
        .collect();

    let list = SourceList { displays, windows };
    let cache = ContentCache {
        displays: raw_displays,
        windows: raw_windows,
    };
    Ok((list, cache))
}

/// Build an `SCContentFilter` from the cached enumeration and the user's
/// pick, then run an `SCStream` until `stop` is set. Replaces the
/// `SCContentSharingPicker::show()` path entirely — no system picker, no
/// `valueForKey:` introspection, no `NSUnknownKeyException`.
fn start_capture(
    cache: ContentCache,
    selection: Selection,
    tx: mpsc::Sender<Wire>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    use screencapturekit::stream::configuration::pixel_format::PixelFormat;
    use screencapturekit::stream::configuration::SCStreamConfiguration;
    use screencapturekit::stream::content_filter::SCContentFilter;
    use screencapturekit::stream::output_type::SCStreamOutputType;
    use screencapturekit::stream::sc_stream::SCStream;

    let (filter, width, height) = match selection {
        Selection::Display { id } => {
            let display = cache
                .displays
                .iter()
                .find(|d| d.display_id() == id)
                .ok_or_else(|| anyhow!("display {id} no longer available"))?;
            let filter = SCContentFilter::create().with_display(display).build();
            (filter, display.width(), display.height())
        }
        Selection::Window { id } => {
            let window = cache
                .windows
                .iter()
                .find(|w| w.window_id() == id)
                .ok_or_else(|| anyhow!("window {id} closed before capture started"))?;
            let filter = SCContentFilter::create().with_window(window).build();
            let frame = window.frame();
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let w = frame.width as u32;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let h = frame.height as u32;
            (filter, w, h)
        }
    };

    // Round dimensions down to even for VP8 + I420 chroma alignment.
    let width = width & !1;
    let height = height & !1;
    if width == 0 || height == 0 {
        return Err(anyhow!("selected source reported zero size"));
    }
    eprintln!("[capture-mac] capturing {width}x{height}");

    let config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_shows_cursor(true);
    let mut stream = SCStream::new(&filter, &config);

    // Announce the format. The parent creates the LiveKit track from
    // this exactly as it does for the Linux Format message.
    tx.blocking_send(Wire::Bytes(encode_format(width, height)))
        .map_err(|_| anyhow!("parent gone before format"))?;

    let handler = MacOsFrameHandler {
        tx: tx.clone(),
        stop: Arc::clone(&stop),
        seen_first: std::sync::atomic::AtomicBool::new(false),
    };
    let _handler_id = stream.add_output_handler(handler, SCStreamOutputType::Screen);
    stream
        .start_capture()
        .map_err(|e| anyhow!("SCStream::start_capture: {e}"))?;
    eprintln!("[capture-mac] SCStream capture started");

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    eprintln!("[capture-mac] stopping SCStream");
    let _ = stream.stop_capture();
    Ok(())
}

struct MacOsFrameHandler {
    tx: mpsc::Sender<Wire>,
    stop: Arc<AtomicBool>,
    /// Set the first time a frame is delivered. Used purely for a
    /// one-shot diagnostic log — proves SCK is actually firing the
    /// output callback (vs the parent showing a black/empty preview
    /// because no frames ever arrived).
    seen_first: std::sync::atomic::AtomicBool,
}

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
        if self.stop.load(Ordering::Relaxed) {
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
        if !self.seen_first.swap(true, Ordering::Relaxed) {
            eprintln!(
                "[capture-mac] first frame delivered: {width}x{height} stride={stride} bytes={}",
                bgra.len()
            );
        }
        let timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);

        // BGRA == little-endian ARGB == the BGRx the parent's
        // argb_to_i420 expects. Pack via the shared protocol encoder so
        // the wire bytes are defined in exactly one place.
        let header = encode_frame_header(width, height, stride, timestamp_us, bgra.len());
        // Last-frame-wins: try_send drops when the socket can't keep up,
        // never blocks SCK's dispatch queue.
        let _ = self.tx.try_send(Wire::Frame {
            header,
            bgrx: bgra.to_vec(),
        });
        drop(guard);
    }
}
