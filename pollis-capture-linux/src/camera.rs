//! Webcam capture (V4L2) for `pollis-capture-linux`.
//!
//! The Linux counterpart of `pollis-capture-macos`'s `camera.rs`. Where
//! the screen path (`linux.rs`) has to fork on session type — Wayland
//! needs the xdg-desktop-portal + PipeWire, X11 needs xcb/SHM — the
//! camera path has **no such split**: V4L2 is a kernel API, so the same
//! code captures a webcam identically under X11, Wayland, or a headless
//! session. (A Flatpak/Snap sandbox would need the camera *portal*, but
//! native Pollis isn't sandboxed — direct `/dev/videoN` access is the
//! native-app convention Discord/Zoom/Chrome use.)
//!
//! Handshake (shared `pollis-capture-proto`, identical to macOS):
//!   1. enumerate capture devices and send the list (`Cameras`, 0x05).
//!      We list every `/dev/videoN` node that reports a VIDEO_CAPTURE
//!      capability *and* at least one pixel format — that filters out the
//!      metadata-only sibling nodes UVC cameras expose (e.g. `/dev/video1`)
//!      without doing any virtual-camera filtering (Discord/Zoom show
//!      everything else).
//!   2. wait for the parent's `SelectCamera` (0x06) carrying the chosen
//!      node path verbatim.
//!   3. open the device, negotiate a format, and stream `Format` (0x01) +
//!      `Frame` (0x02) BGRx frames until the parent closes the socket.
//!
//! Pixel format: webcams rarely speak BGRx natively. We prefer **MJPG**
//! (the only HD format many UVC cameras expose — some are MJPG-only),
//! decode it to RGB with `zune-jpeg`, and pack BGRx. We fall back to raw
//! **YUYV** (4:2:2) for cameras without MJPG, converting in-process. H.264
//! is intentionally ignored — decoding it would pull in a heavy codec for
//! no gain, and every UVC camera also offers MJPG or YUYV. The parent's
//! shared `argb_to_i420` + LiveKit publish is unchanged — it only ever
//! sees BGRx, exactly like the screen and macOS-camera paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use pollis_capture_proto::{
    read_msg, CameraList, CameraSource, CaptureMsg,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

use v4l::buffer::Type as BufType;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

/// We prefer a 720p capture: a sane video-call default that every webcam
/// supports, lighter on USB bandwidth and JPEG-decode CPU than 1080p, and
/// the parent re-encodes to VP8 at a bitrate tuned to the negotiated size
/// anyway. The driver adjusts to its nearest supported size; we publish
/// whatever it actually gives us.
const TARGET_WIDTH: u32 = 1280;
const TARGET_HEIGHT: u32 = 720;

/// How long to wait for the parent's `SelectCamera` after we send the
/// device list. Matches the macOS helper — generous to cover the user
/// taking their time in the picker.
const SELECT_TIMEOUT: Duration = Duration::from_secs(300);

/// Entry point for `--mode camera`. Mirrors the macOS `run_camera`: speaks
/// the same enumerate → select → stream handshake over the parent socket.
pub async fn run_camera(sock: &mut UnixStream) -> Result<()> {
    use pollis_capture_proto::write_msg;

    // ── Phase 1: enumerate cameras + send the list ──────────────────────
    eprintln!("[capture/cam] enumerating V4L2 capture devices");
    let list = match enumerate_cameras() {
        Ok(list) => list,
        Err(e) => {
            let msg = format!("{e}");
            let _ = write_msg(sock, &CaptureMsg::Error { message: msg.clone() }).await;
            return Err(anyhow!(msg));
        }
    };
    eprintln!("[capture/cam] enumerated {} camera(s)", list.cameras.len());
    write_msg(sock, &CaptureMsg::Cameras(list))
        .await
        .context("send Cameras")?;

    // ── Phase 2: wait for the user's pick (SelectCamera) ────────────────
    let device_id = match timeout(SELECT_TIMEOUT, read_msg(sock)).await {
        Ok(Ok(Some(CaptureMsg::SelectCamera(sel)))) => sel.id,
        Ok(Ok(Some(CaptureMsg::Select(_)))) => {
            return Err(anyhow!("received screen Select while in camera mode"));
        }
        Ok(Ok(Some(other))) => {
            return Err(anyhow!("unexpected message before SelectCamera: {other:?}"));
        }
        Ok(Ok(None)) => {
            eprintln!("[capture/cam] parent closed socket before SelectCamera — exiting");
            return Ok(());
        }
        Ok(Err(e)) => return Err(anyhow!("read SelectCamera: {e}")),
        Err(_) => return Err(anyhow!("timed out waiting for camera selection")),
    };
    eprintln!("[capture/cam] selected camera: {device_id}");

    // ── Phase 3: open the device + stream frames ────────────────────────
    //
    // V4L2 is a synchronous, blocking ioctl API, so capture runs on its
    // own OS thread and feeds frames back over a bounded channel — the
    // exact pattern the portal (pipewire) and X11 (xcb) screen backends
    // use. Capacity 2 + last-frame-wins keeps the hot path from blocking
    // when the socket can't keep up.
    let (tx, mut rx) = mpsc::channel::<CaptureMsg>(2);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);

    let cap_thread = std::thread::Builder::new()
        .name("pollis-capture-cam".into())
        .spawn(move || {
            eprintln!("[capture/cam] capture thread entered");
            if let Err(e) = capture_loop(&device_id, &tx, &stop_for_thread) {
                eprintln!("[capture/cam] error: {e}");
                let _ = tx.blocking_send(CaptureMsg::Error {
                    message: format!("camera: {e}"),
                });
            }
            eprintln!("[capture/cam] capture thread exiting");
        })
        .context("spawn camera thread")?;

    // Drain channel → socket until the parent goes away (write error) or
    // the capture thread ends.
    let result: Result<()> = async {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = write_msg(sock, &msg).await {
                eprintln!("[capture/cam] socket write error: {e} — exiting");
                break;
            }
        }
        Ok(())
    }
    .await;

    stop.store(true, Ordering::Relaxed);
    drop(cap_thread);
    result
}

/// Enumerate V4L2 capture devices. Lists every `/dev/videoN` node that
/// advertises a VIDEO_CAPTURE capability and enumerates at least one pixel
/// format — the format check drops the metadata-only nodes UVC cameras
/// expose alongside the real capture node. No virtual-camera filtering;
/// the parent shows whatever's left (Discord/Zoom convention).
fn enumerate_cameras() -> Result<CameraList> {
    let mut cameras = Vec::new();
    for node in v4l::context::enum_devices() {
        let path = node.path().to_path_buf();
        let Ok(dev) = Device::with_path(&path) else {
            continue;
        };
        // Must be a capture device…
        match dev.query_caps() {
            Ok(caps)
                if caps
                    .capabilities
                    .contains(v4l::capability::Flags::VIDEO_CAPTURE) => {}
            _ => continue,
        }
        // …that actually offers a capture format (filters metadata nodes).
        let has_format = Capture::enum_formats(&dev)
            .map(|f| !f.is_empty())
            .unwrap_or(false);
        if !has_format {
            continue;
        }

        let id = path.to_string_lossy().into_owned();
        // Prefer the human card name; fall back to the node path.
        let name = node
            .name()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        cameras.push(CameraSource { id, name });
    }
    Ok(CameraList { cameras })
}

/// Which decode path a negotiated format needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pixel {
    /// Motion-JPEG — decode each buffer with zune-jpeg to RGB.
    Mjpg,
    /// Raw YUYV 4:2:2 (a.k.a. YUV422) — convert in place.
    Yuyv,
}

/// Open the chosen device, negotiate a format we can convert to BGRx, and
/// pump frames into `tx` until `stop` is set or the socket closes.
fn capture_loop(
    device_id: &str,
    tx: &mpsc::Sender<CaptureMsg>,
    stop: &AtomicBool,
) -> Result<()> {
    let dev = Device::with_path(device_id)
        .with_context(|| format!("open camera {device_id}"))?;

    // Pick the best format the device supports: MJPG first (universal HD,
    // sometimes the only option), then raw YUYV.
    let formats = Capture::enum_formats(&dev).context("enumerate formats")?;
    let supports = |fourcc: &[u8; 4]| formats.iter().any(|f| f.fourcc == FourCC::new(fourcc));
    let (fourcc, pixel) = if supports(b"MJPG") {
        (FourCC::new(b"MJPG"), Pixel::Mjpg)
    } else if supports(b"YUYV") {
        (FourCC::new(b"YUYV"), Pixel::Yuyv)
    } else {
        return Err(anyhow!(
            "camera offers no MJPG or YUYV format (only {:?}) — unsupported",
            formats.iter().map(|f| f.fourcc.to_string()).collect::<Vec<_>>()
        ));
    };

    let mut fmt = Capture::format(&dev).context("read current format")?;
    fmt.fourcc = fourcc;
    fmt.width = TARGET_WIDTH;
    fmt.height = TARGET_HEIGHT;
    let fmt = Capture::set_format(&dev, &fmt).context("set format")?;
    eprintln!(
        "[capture/cam] negotiated {}x{} {} ({:?})",
        fmt.width, fmt.height, fmt.fourcc, pixel
    );
    if fmt.fourcc != fourcc {
        return Err(anyhow!(
            "driver refused {fourcc}, gave {} — unsupported",
            fmt.fourcc
        ));
    }

    // Ask for 30fps; harmless if the driver ignores it (the parent caps
    // the publish rate regardless).
    let mut params = Capture::params(&dev).context("read params")?;
    params.interval = v4l::Fraction::new(1, 30);
    let _ = Capture::set_params(&dev, &params);

    let width = fmt.width;
    let height = fmt.height;
    // Even dimensions for I420 chroma alignment, matching the screen +
    // macOS-camera paths. The parent rounds too, but announce even.
    let even_w = width & !1;
    let even_h = height & !1;
    if even_w == 0 || even_h == 0 {
        return Err(anyhow!("camera reported a zero dimension"));
    }
    let stride = even_w * 4;

    // mmap streaming with a small buffer ring.
    let mut stream = v4l::io::mmap::Stream::with_buffers(&dev, BufType::VideoCapture, 4)
        .context("start mmap stream")?;

    // BGRx scratch reused across frames so the hot loop doesn't reallocate.
    let mut bgrx = vec![0u8; (stride as usize) * (even_h as usize)];
    let mut announced = false;

    while !stop.load(Ordering::Relaxed) {
        let (buf, meta) = match stream.next() {
            Ok(v) => v,
            Err(e) => return Err(anyhow!("dequeue frame: {e}")),
        };
        let used = (meta.bytesused as usize).min(buf.len());
        let data = &buf[..used];

        let ok = match pixel {
            Pixel::Yuyv => yuyv_to_bgrx(data, even_w, even_h, &mut bgrx),
            Pixel::Mjpg => mjpg_to_bgrx(data, even_w, even_h, &mut bgrx),
        };
        if !ok {
            // A corrupt/short frame (rare MJPG hiccup, or a warm-up frame
            // before the sensor settles). Skip it — the next one is along
            // in ~33ms.
            continue;
        }

        if !announced {
            announced = true;
            eprintln!("[capture/cam] first frame {even_w}x{even_h} stride={stride}");
            if tx
                .blocking_send(CaptureMsg::Format {
                    width: even_w,
                    height: even_h,
                })
                .is_err()
            {
                break;
            }
        }

        // Last-frame-wins: try_send drops when the socket can't keep up,
        // never blocking the capture thread.
        let _ = tx.try_send(CaptureMsg::Frame {
            width: even_w,
            height: even_h,
            stride,
            timestamp_us: now_us(),
            bgrx: bgrx.clone(),
        });
    }
    Ok(())
}

/// Convert a packed YUYV (4:2:2) buffer to BGRx. Returns false if the
/// source is too short for the claimed dimensions. Two pixels share one
/// U/V pair: `[Y0 U Y1 V]` → two BGRx quads. BGRx == little-endian ARGB,
/// the byte order the parent's `argb_to_i420` reads.
fn yuyv_to_bgrx(src: &[u8], width: u32, height: u32, dst: &mut [u8]) -> bool {
    let w = width as usize;
    let h = height as usize;
    if src.len() < w * h * 2 || dst.len() < w * h * 4 {
        return false;
    }
    for row in 0..h {
        let s_row = &src[row * w * 2..];
        let d_row = &mut dst[row * w * 4..];
        let mut x = 0;
        while x + 1 < w {
            let i = x * 2;
            let y0 = s_row[i] as i32;
            let u = s_row[i + 1] as i32;
            let y1 = s_row[i + 2] as i32;
            let v = s_row[i + 3] as i32;
            write_bgrx(&mut d_row[x * 4..], y0, u, v);
            write_bgrx(&mut d_row[(x + 1) * 4..], y1, u, v);
            x += 2;
        }
    }
    true
}

/// Decode an MJPG buffer to RGB (`zune-jpeg`) and pack BGRx. Returns false
/// on a decode error or a dimension mismatch — both are recoverable by
/// skipping the frame.
fn mjpg_to_bgrx(src: &[u8], width: u32, height: u32, dst: &mut [u8]) -> bool {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    // zune-jpeg 0.5's reader trait wants BufRead+Seek (or its own
    // ZCursor); a bare &[u8] doesn't qualify, so wrap it.
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(src), opts);
    let rgb: Vec<u8> = match decoder.decode() {
        Ok(px) => px,
        Err(_) => return false,
    };
    let Some(info) = decoder.info() else {
        return false;
    };
    let w = width as usize;
    let h = height as usize;
    // The driver-negotiated size should match the JPEG's; bail (skip the
    // frame) rather than read out of bounds if a stray frame disagrees.
    if info.width as usize != w || info.height as usize != h {
        return false;
    }
    if rgb.len() < w * h * 3 || dst.len() < w * h * 4 {
        return false;
    }
    for px in 0..(w * h) {
        let r = rgb[px * 3] as u32;
        let g = rgb[px * 3 + 1] as u32;
        let b = rgb[px * 3 + 2] as u32;
        let d = &mut dst[px * 4..];
        d[0] = b as u8;
        d[1] = g as u8;
        d[2] = r as u8;
        d[3] = 0xFF;
    }
    true
}

/// BT.601 limited-range YUV → BGRx, written into the first 4 bytes of
/// `dst`. Integer approximation, matching the coefficients libyuv uses on
/// the conversion side.
#[inline]
fn write_bgrx(dst: &mut [u8], y: i32, u: i32, v: i32) {
    let c = y - 16;
    let d = u - 128;
    let e = v - 128;
    let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255);
    let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255);
    let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255);
    dst[0] = b as u8;
    dst[1] = g as u8;
    dst[2] = r as u8;
    dst[3] = 0xFF;
}

fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A flat gray YUYV frame (Y=U=V=128) round-trips to a flat gray BGRx
    // frame with opaque alpha. Pins both the BT.601 math and the 4-byte
    // BGRx packing.
    #[test]
    fn yuyv_gray_to_bgrx() {
        // 2x2, packed YUYV is 2 bytes/pixel: [Y0 U Y1 V] per pixel pair.
        let src = vec![128u8; 2 * 2 * 2];
        let mut dst = vec![0u8; 2 * 2 * 4];
        assert!(yuyv_to_bgrx(&src, 2, 2, &mut dst));
        for px in dst.chunks_exact(4) {
            // Y=U=V=128 → ~130 on every channel; alpha forced opaque.
            assert_eq!(px[0], 130, "B");
            assert_eq!(px[1], 130, "G");
            assert_eq!(px[2], 130, "R");
            assert_eq!(px[3], 0xFF, "A");
        }
    }

    // A pure-red YUV sample must land in the R slot, not B — guards against
    // a swapped channel order (the classic BGR/RGB bug).
    #[test]
    fn write_bgrx_red_channel_order() {
        let mut dst = [0u8; 4];
        // BT.601 "red": Y≈81, U≈90, V≈240.
        write_bgrx(&mut dst, 81, 90, 240);
        assert_eq!(dst[2], 255, "R should be saturated");
        assert_eq!(dst[0], 0, "B should be zero");
        assert_eq!(dst[3], 0xFF, "alpha opaque");
    }

    // A source too short for the claimed dimensions is rejected, not read
    // out of bounds.
    #[test]
    fn yuyv_short_src_rejected() {
        let src = vec![0u8; 4]; // far too small for 4x4
        let mut dst = vec![0u8; 4 * 4 * 4];
        assert!(!yuyv_to_bgrx(&src, 4, 4, &mut dst));
    }

    // Garbage handed to the MJPG path is a skippable frame, never a panic.
    #[test]
    fn mjpg_garbage_rejected() {
        let src = [0xDEu8, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
        let mut dst = vec![0u8; 2 * 2 * 4];
        assert!(!mjpg_to_bgrx(&src, 2, 2, &mut dst));
    }
}
