//! Codec choice, I420 conversion and frame-bytes packing, shared by push and drain.

use libwebrtc::video_frame::I420Buffer;
use livekit::options::VideoCodec;

/// Picks the codec used to publish the local screen-share track.
///
/// VP8 by default: cheapest SW encoder; LiveKit caps H.264 at 720p (opt-in).
///
/// `POLLIS_SCREENSHARE_CODEC` (`vp8|h264|vp9|av1|h265`) overrides; else VP8.
pub(super) fn pick_screenshare_codec() -> VideoCodec {
    if let Ok(v) = std::env::var("POLLIS_SCREENSHARE_CODEC") {
        match v.to_ascii_lowercase().as_str() {
            "vp8" => return VideoCodec::VP8,
            "h264" => return VideoCodec::H264,
            "vp9" => return VideoCodec::VP9,
            "av1" => return VideoCodec::AV1,
            "h265" => return VideoCodec::H265,
            _ => {}
        }
    }
    VideoCodec::VP8
}

/// Resolve the user's Screen Share framerate preference into the
/// `(max_framerate, max_bitrate)` pair fed to `VideoEncoding` on publish.
///
/// Default 30 fps, clamped to 1..=60 (higher raced capture into a tokio panic).
pub(super) fn resolve_screenshare_encoding(max_framerate: Option<u32>) -> (f64, u64) {
    let fps = max_framerate.unwrap_or(30).clamp(1, 60);
    let max_bitrate = fps as u64 * 130_000;
    (fps as f64, max_bitrate)
}

/// `argb_to_i420` into a fresh I420 buffer at native size (no downscale).
pub(crate) fn convert_to_i420(
    width: i32,
    height: i32,
    src_stride: u32,
    argb: &[u8],
) -> I420Buffer {
    let mut buffer = I420Buffer::new(width as u32, height as u32);
    {
        let (sy, su, sv) = buffer.strides();
        let (dy, du, dv) = buffer.data_mut();
        libwebrtc::native::yuv_helper::argb_to_i420(
            argb, src_stride, dy, sy, du, su, dv, sv, width, height,
        );
    }
    buffer
}

// Frame wire format (Rust -> webview):
// [ u32 LE track_key_len ][ track_key UTF-8 ]
// [ u32 LE width ][ u32 LE height ]
// [ u32 LE y_stride ][ u32 LE u_stride ][ u32 LE v_stride ]
// [ i64 LE timestamp_us ]
// [ Y plane bytes ][ U plane bytes ][ V plane bytes ]
pub(crate) fn pack_frame_bytes(
    track_key: &str,
    width: u32,
    height: u32,
    timestamp_us: i64,
    i420: &I420Buffer,
) -> Vec<u8> {
    let (y_stride, u_stride, v_stride) = i420.strides();
    let (y, u, v) = i420.data();
    let header_len = 4 + track_key.len() + 4 + 4 + 4 + 4 + 4 + 8;
    let mut out = Vec::with_capacity(header_len + y.len() + u.len() + v.len());
    out.extend_from_slice(&(track_key.len() as u32).to_le_bytes());
    out.extend_from_slice(track_key.as_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&y_stride.to_le_bytes());
    out.extend_from_slice(&u_stride.to_le_bytes());
    out.extend_from_slice(&v_stride.to_le_bytes());
    out.extend_from_slice(&timestamp_us.to_le_bytes());
    out.extend_from_slice(y);
    out.extend_from_slice(u);
    out.extend_from_slice(v);
    out
}
