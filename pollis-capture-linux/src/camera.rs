//! Webcam capture (V4L2) for `pollis-capture-linux`.
//!
//! V4L2 is a kernel API, so one code path covers X11, Wayland and headless.
//!
//! Handshake (shared `pollis-capture-proto`, identical to macOS):
//!   1. enumerate capture devices and send the list (`Cameras`, 0x05).
//!      Metadata-only UVC nodes are dropped; no virtual-camera filtering.
//!   2. wait for the parent's `SelectCamera` (0x06) carrying the chosen
//!      node path verbatim.
//!   3. open the device, negotiate a format, and stream `Format` (0x01) +
//!      `FrameI420` (0x09) frames until the parent closes the socket.
//!
//! Pixel format: prefer MJPG, fall back to YUYV; H.264 is ignored. Both are YUV already,
//! so the output is tight I420 planes — the parent copies them into the encoder's buffer
//! and never converts (BGRx cost a 4-byte-per-pixel pack here and a libyuv pass there).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use pollis_capture_proto::{
    i420_plane_lengths, now_us, read_msg, CameraList, CameraSource, CaptureMsg,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

use v4l::buffer::Type as BufType;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

/// 720p target; the driver adjusts to its nearest supported size.
const TARGET_WIDTH: u32 = 1280;
const TARGET_HEIGHT: u32 = 720;

/// How long to wait for `SelectCamera`; generous for a slow pick.
const SELECT_TIMEOUT: Duration = Duration::from_secs(300);

/// Entry point for `--mode camera`.
pub async fn run_camera(sock: &mut UnixStream) -> Result<()> {
    use pollis_capture_proto::write_msg;

    eprintln!("[capture/cam] enumerating V4L2 capture devices");
    let list = match enumerate_cameras() {
        Ok(list) => list,
        Err(e) => {
            let msg = format!("{e}");
            let _ = write_msg(
                sock,
                &CaptureMsg::Error {
                    message: msg.clone(),
                },
            )
            .await;
            return Err(anyhow!(msg));
        }
    };
    eprintln!("[capture/cam] enumerated {} camera(s)", list.cameras.len());
    write_msg(sock, &CaptureMsg::Cameras(list))
        .await
        .context("send Cameras")?;

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

    // V4L2 blocks, so capture runs on its own thread; capacity 2, last-frame-wins.
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

    crate::linux::drain_to_socket(sock, &mut rx, "[capture/cam]").await;

    stop.store(true, Ordering::Relaxed);
    drop(cap_thread);
    Ok(())
}

/// Enumerate `/dev/videoN` capture nodes that offer a pixel format.
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
    /// Motion-JPEG — zune-jpeg decodes to YCbCr, subsampled 2×2 into I420.
    Mjpg,
    /// Raw YUYV 4:2:2 — split into planes, chroma averaged over each row pair.
    Yuyv,
}

/// Open the chosen device, negotiate a format we can convert to BGRx, and
/// pump frames into `tx` until `stop` is set or the socket closes.
fn capture_loop(device_id: &str, tx: &mpsc::Sender<CaptureMsg>, stop: &AtomicBool) -> Result<()> {
    let dev = Device::with_path(device_id).with_context(|| format!("open camera {device_id}"))?;

    // MJPG first (some HD cameras are MJPG-only), then YUYV.
    let formats = Capture::enum_formats(&dev).context("enumerate formats")?;
    let supports = |fourcc: &[u8; 4]| formats.iter().any(|f| f.fourcc == FourCC::new(fourcc));
    let (fourcc, pixel) = if supports(b"MJPG") {
        (FourCC::new(b"MJPG"), Pixel::Mjpg)
    } else if supports(b"YUYV") {
        (FourCC::new(b"YUYV"), Pixel::Yuyv)
    } else {
        return Err(anyhow!(
            "camera offers no MJPG or YUYV format (only {:?}) — unsupported",
            formats
                .iter()
                .map(|f| f.fourcc.to_string())
                .collect::<Vec<_>>()
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

    // Ask for 30fps; harmless if ignored.
    let mut params = Capture::params(&dev).context("read params")?;
    params.interval = v4l::Fraction::new(1, 30);
    let _ = Capture::set_params(&dev, &params);

    let width = fmt.width;
    let height = fmt.height;
    // Even dimensions for I420 chroma alignment.
    let even_w = width & !1;
    let even_h = height & !1;
    if even_w == 0 || even_h == 0 {
        return Err(anyhow!("camera reported a zero dimension"));
    }

    let mut stream = v4l::io::mmap::Stream::with_buffers(&dev, BufType::VideoCapture, 4)
        .context("start mmap stream")?;

    // Planes scratch reused across frames; the MJPG path also keeps zune's YCbCr output.
    let (y_len, c_len) = i420_plane_lengths(even_w, even_h);
    let mut planes = vec![0u8; y_len + 2 * c_len];
    let mut ycbcr: Vec<u8> = Vec::new();
    let mut announced = false;

    while !stop.load(Ordering::Relaxed) {
        let (buf, meta) = match stream.next() {
            Ok(v) => v,
            Err(e) => return Err(anyhow!("dequeue frame: {e}")),
        };
        // Stamp at dequeue so decode/convert time counts as latency.
        let captured_us = now_us();
        let used = (meta.bytesused as usize).min(buf.len());
        let data = &buf[..used];

        let ok = match pixel {
            Pixel::Yuyv => yuyv_to_i420(data, even_w, even_h, &mut planes),
            Pixel::Mjpg => mjpg_to_i420(data, even_w, even_h, &mut ycbcr, &mut planes),
        };
        if !ok {
            // A corrupt/short frame (MJPG hiccup or warm-up): skip it.
            continue;
        }

        if !announced {
            announced = true;
            eprintln!("[capture/cam] first frame {even_w}x{even_h} (i420)");
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

        // Last-frame-wins; never block the capture thread.
        let _ = tx.try_send(CaptureMsg::FrameI420 {
            width: even_w,
            height: even_h,
            timestamp_us: captured_us,
            planes: planes.clone(),
        });
    }
    Ok(())
}

/// Packed YUYV 4:2:2 → tight I420 planes; false if `src` is too short. Chroma is averaged
/// over the two rows of each 2×2 block (plain decimation would alias on fine detail).
fn yuyv_to_i420(src: &[u8], width: u32, height: u32, planes: &mut [u8]) -> bool {
    let w = width as usize;
    let h = height as usize;
    let cw = w / 2;
    let (y_len, c_len) = i420_plane_lengths(width, height);
    if src.len() < w * h * 2 || planes.len() < y_len + 2 * c_len {
        return false;
    }
    let (y, uv) = planes.split_at_mut(y_len);
    let (u, v) = uv.split_at_mut(c_len);
    for row in 0..h {
        let s = &src[row * w * 2..row * w * 2 + w * 2];
        let yrow = &mut y[row * w..row * w + w];
        for (x, out) in yrow.iter_mut().enumerate() {
            *out = s[x * 2];
        }
    }
    for crow in 0..h / 2 {
        let s0 = &src[(crow * 2) * w * 2..];
        let s1 = &src[(crow * 2 + 1) * w * 2..];
        let urow = &mut u[crow * cw..crow * cw + cw];
        let vrow = &mut v[crow * cw..crow * cw + cw];
        for cx in 0..cw {
            let i = cx * 4;
            urow[cx] = (s0[i + 1] as u16 + s1[i + 1] as u16).div_ceil(2) as u8;
            vrow[cx] = (s0[i + 3] as u16 + s1[i + 3] as u16).div_ceil(2) as u8;
        }
    }
    true
}

/// Interleaved 4:4:4 YCbCr (zune's output, chroma already upsampled) → tight I420 planes,
/// chroma averaged over each 2×2 block.
fn ycbcr_to_i420(src: &[u8], width: u32, height: u32, planes: &mut [u8]) -> bool {
    let w = width as usize;
    let h = height as usize;
    let cw = w / 2;
    let (y_len, c_len) = i420_plane_lengths(width, height);
    if src.len() < w * h * 3 || planes.len() < y_len + 2 * c_len {
        return false;
    }
    let (y, uv) = planes.split_at_mut(y_len);
    let (u, v) = uv.split_at_mut(c_len);
    for row in 0..h {
        let s = &src[row * w * 3..row * w * 3 + w * 3];
        let yrow = &mut y[row * w..row * w + w];
        for (x, out) in yrow.iter_mut().enumerate() {
            *out = s[x * 3];
        }
    }
    for crow in 0..h / 2 {
        let s0 = &src[(crow * 2) * w * 3..];
        let s1 = &src[(crow * 2 + 1) * w * 3..];
        let urow = &mut u[crow * cw..crow * cw + cw];
        let vrow = &mut v[crow * cw..crow * cw + cw];
        for cx in 0..cw {
            let i = cx * 6;
            urow[cx] =
                ((s0[i + 1] as u16 + s0[i + 4] as u16 + s1[i + 1] as u16 + s1[i + 4] as u16 + 2)
                    / 4) as u8;
            vrow[cx] =
                ((s0[i + 2] as u16 + s0[i + 5] as u16 + s1[i + 2] as u16 + s1[i + 5] as u16 + 2)
                    / 4) as u8;
        }
    }
    true
}

/// Decode MJPG to YCbCr (no RGB pass) and subsample into I420; false on a skippable bad
/// frame. `ycbcr` is the decoder's output scratch, reused across frames.
fn mjpg_to_i420(
    src: &[u8],
    width: u32,
    height: u32,
    ycbcr: &mut Vec<u8>,
    planes: &mut [u8],
) -> bool {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::YCbCr);
    // zune-jpeg 0.5 needs BufRead+Seek; wrap the slice.
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(src), opts);
    if decoder.decode_headers().is_err() {
        return false;
    }
    let Some(info) = decoder.info() else {
        return false;
    };
    // Skip a frame whose JPEG size disagrees rather than read out of bounds.
    if info.width as u32 != width || info.height as u32 != height {
        return false;
    }
    let Some(need) = decoder.output_buffer_size() else {
        return false;
    };
    ycbcr.resize(need, 0);
    if decoder.decode_into(ycbcr).is_err() {
        return false;
    }
    ycbcr_to_i420(ycbcr, width, height, planes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Flat gray YUYV → flat gray planes; pins the plane layout and chroma averaging.
    #[test]
    fn yuyv_gray_to_i420() {
        // 4x2, packed YUYV is 2 bytes/pixel: [Y0 U Y1 V] per pixel pair.
        let mut src = vec![0u8; 4 * 2 * 2];
        for (i, b) in src.iter_mut().enumerate() {
            *b = match i % 4 {
                0 | 2 => 100,
                1 => 60,
                _ => 200,
            };
        }
        let (y_len, c_len) = i420_plane_lengths(4, 2);
        let mut planes = vec![0u8; y_len + 2 * c_len];
        assert!(yuyv_to_i420(&src, 4, 2, &mut planes));
        assert!(planes[..y_len].iter().all(|&b| b == 100), "Y");
        assert!(planes[y_len..y_len + c_len].iter().all(|&b| b == 60), "U");
        assert!(planes[y_len + c_len..].iter().all(|&b| b == 200), "V");
    }

    // The two rows of a chroma block are averaged, not decimated.
    #[test]
    fn yuyv_chroma_averages_row_pairs() {
        // 2x2: row 0 U=10, row 1 U=30 → 20.
        let src = [50u8, 10, 50, 0, 50, 30, 50, 0];
        let mut planes = vec![0u8; 4 + 1 + 1];
        assert!(yuyv_to_i420(&src, 2, 2, &mut planes));
        assert_eq!(planes[4], 20, "U");
        assert_eq!(planes[5], 0, "V");
    }

    // A source too short for the claimed dimensions is rejected, not read out of bounds.
    #[test]
    fn yuyv_short_src_rejected() {
        let src = vec![0u8; 4];
        let mut planes = vec![0u8; 4 * 4 * 3 / 2];
        assert!(!yuyv_to_i420(&src, 4, 4, &mut planes));
    }

    #[test]
    fn ycbcr_444_subsamples_each_block() {
        // 2x2 interleaved YCbCr; Y distinct per pixel, Cb 8/12/16/20 → 14, Cr all 7.
        let src = [1u8, 8, 7, 2, 12, 7, 3, 16, 7, 4, 20, 7];
        let mut planes = vec![0u8; 6];
        assert!(ycbcr_to_i420(&src, 2, 2, &mut planes));
        assert_eq!(&planes[..4], &[1, 2, 3, 4]);
        assert_eq!(planes[4], 14);
        assert_eq!(planes[5], 7);
    }

    // Garbage handed to the MJPG path is a skippable frame, never a panic.
    #[test]
    fn mjpg_garbage_rejected() {
        let src = [0xDEu8, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03];
        let mut planes = vec![0u8; 6];
        let mut scratch = Vec::new();
        assert!(!mjpg_to_i420(&src, 2, 2, &mut scratch, &mut planes));
    }
}
