use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use pollis_capture_proto::{encode_error, encode_format, encode_frame_header};
use tokio::io::AsyncWriteExt;
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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move { run_async(&args.socket).await })
}

async fn run_async(socket_path: &str) -> Result<()> {
    eprintln!("[capture-mac] connecting to parent socket {socket_path}");
    let mut sock = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to {socket_path}"))?;
    eprintln!("[capture-mac] connected — showing SCK picker");

    // macOS has no PR_SET_PDEATHSIG. Poll getppid(): if the parent dies
    // it reparents to launchd (ppid becomes 1) — exit so we don't leak a
    // live SCStream. Cheap, 1 s cadence.
    spawn_parent_death_watch();

    let (tx, mut rx) = mpsc::channel::<Wire>(2);
    let stop = Arc::new(AtomicBool::new(false));

    // Start SCK on a dedicated blocking context: the picker callback and
    // SCStream start are Swift FFI calls. Errors before the first frame
    // are sent as 0xFF and end the run.
    let stop_for_cap = Arc::clone(&stop);
    let tx_for_cap = tx.clone();
    let cap_handle = tokio::task::spawn_blocking(move || {
        if let Err(e) = start_capture(tx_for_cap.clone(), stop_for_cap) {
            eprintln!("[capture-mac] capture error: {e}");
            // Best-effort error relay; the parent maps 0xFF to a
            // structured capture failure.
            let _ = tx_for_cap.blocking_send(Wire::Bytes(encode_error(&format!(
                "screencapturekit: {e}"
            ))));
        }
    });

    // Drain channel -> socket. On any write error (EPIPE, parent gone)
    // stop and exit; the SCK side observes `stop` on its next frame.
    let result: Result<()> = async {
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
        Ok(())
    }
    .await;

    stop.store(true, Ordering::Relaxed);
    cap_handle.abort();
    result
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

/// Show the system content-sharing picker, build the SCStream around the
/// picked filter, and stream frames to `tx` as shared-protocol messages.
/// Blocking (Swift FFI) — called from `spawn_blocking`.
///
/// This is the SCK picker/stream/handler logic extracted verbatim (logic
/// preserved) from `pollis-core/src/commands/screenshare.rs`'s macOS
/// `start_screen_share` + `MacOsFrameHandler`. The only change: instead
/// of pushing into a LiveKit `NativeVideoSource`, the handler now packs
/// BGRx into the shared protocol and sends it over the socket. The
/// parent reconstructs the LiveKit publish from the Format message,
/// exactly as it already does for the Linux helper.
fn start_capture(tx: mpsc::Sender<Wire>, stop: Arc<AtomicBool>) -> Result<()> {
    use screencapturekit::content_sharing_picker::{
        SCContentSharingPicker, SCContentSharingPickerConfiguration,
        SCContentSharingPickerMode, SCPickerOutcome,
    };
    use screencapturekit::prelude::*;

    // 1. Show the macOS system content-sharing picker. show() returns
    //    immediately; Swift fires the callback on the main run loop when
    //    the user makes a selection.
    //
    //    UNVERIFIED (issue #283 Phase 0 spike, out of scope): whether
    //    this picker presents from a *helper* process with no app
    //    activation policy / window-server foreground state. If it does
    //    not, the parent must drive the picker and hand us a selected
    //    filter instead. Flagged in the crate-level docs + wiki.
    let mut picker_config = SCContentSharingPickerConfiguration::new();
    picker_config.set_allowed_picker_modes(&[
        SCContentSharingPickerMode::SingleDisplay,
        SCContentSharingPickerMode::SingleWindow,
        SCContentSharingPickerMode::SingleApplication,
    ]);

    let (ptx, prx) = std::sync::mpsc::channel::<SCPickerOutcome>();
    SCContentSharingPicker::show(&picker_config, move |outcome| {
        let _ = ptx.send(outcome);
    });
    // Block this dedicated thread until the user picks. 5-minute guard
    // mirrors the parent's old 300 s wait for a source.
    let outcome = prx
        .recv_timeout(std::time::Duration::from_secs(300))
        .map_err(|_| anyhow!("picker timed out waiting for a source"))?;

    let picked = match outcome {
        SCPickerOutcome::Picked(p) => p,
        SCPickerOutcome::Cancelled => {
            // User cancellation is a normal flow. Surface it as a
            // recognisable, non-"denied" error string the parent's
            // friendly-error mapping already handles ("cancel").
            return Err(anyhow!("screen share cancelled (picker dismissed)"));
        }
        SCPickerOutcome::Error(msg) => {
            eprintln!("[capture-mac] SCK picker raw error: {msg}");
            return Err(anyhow!(
                "could not open the screen-share picker. Check Screen \
                 Recording permission in System Settings."
            ));
        }
    };

    let filter = picked.filter();
    let (px_w, px_h) = picked.pixel_size();
    // Force even dims for VP8 + I420 chroma alignment (the parent also
    // re-floors, but announcing even keeps Format honest).
    let width = px_w & !1;
    let height = px_h & !1;
    if width == 0 || height == 0 {
        return Err(anyhow!("picker reported zero-size selection"));
    }
    eprintln!("[capture-mac] picked {width}x{height}");

    let config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_shows_cursor(true);
    let mut stream = SCStream::new(&filter, &config);

    // Announce the format. The parent creates the LiveKit track from
    // this exactly as it does for the Linux Format message.
    tx.blocking_send(Wire::Bytes(encode_format(width as u32, height as u32)))
        .map_err(|_| anyhow!("parent gone before format"))?;

    let handler = MacOsFrameHandler {
        tx: tx.clone(),
        stop: Arc::clone(&stop),
    };
    let _handler_id = stream.add_output_handler(handler, SCStreamOutputType::Screen);
    stream
        .start_capture()
        .map_err(|e| anyhow!("SCStream::start_capture: {e}"))?;
    eprintln!("[capture-mac] SCStream capture started");

    // Park here until the parent closes the socket (the drain task sets
    // `stop`) or capture dies. SCK delivers frames on its own dispatch
    // queue via the handler; nothing to do on this thread but wait.
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    eprintln!("[capture-mac] stopping SCStream");
    let _ = stream.stop_capture();
    SCContentSharingPicker::set_active(false);
    Ok(())
}

struct MacOsFrameHandler {
    tx: mpsc::Sender<Wire>,
    stop: Arc<AtomicBool>,
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
        // Unlock the CVPixelBuffer promptly for ScreenCaptureKit.
        drop(guard);
    }
}
