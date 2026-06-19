//! Codec selection + I420 conversion + frame-bytes packing. The shared
//! frame-manipulation primitives used by both the capture push path
//! (Linux/macOS `push_frame`, Windows `push_frame_windows`) and the
//! remote-drain path (`remote_video::on_remote_video_subscribed`).

use libwebrtc::video_frame::I420Buffer;
use livekit::options::VideoCodec;

/// Picks the codec used to publish the local screen-share track.
///
/// Defaults to VP8 on every platform. Reasoning:
/// - VP8 is the cheapest software encoder in libwebrtc, which matches
///   our "as many frames at native resolution as possible" goal for
///   screen share. SW VP9/AV1 trade CPU for compression; SW H.264
///   (OpenH264) is comparable to VP8 but has no decode side benefits
///   for our use case.
/// - The "VideoToolbox H.264" HW-accel path on macOS is real but
///   LiveKit's Rust SDK pins H.264 SDP preferences to profile
///   `42e01f` (Constrained Baseline Level 3.1, 1280×720 ceiling — see
///   `rtc_session.rs::create_sender` in livekit 0.7). Publishing >720p
///   as H.264 silently emits zero RTP packets. That tradeoff (HW accel
///   at the cost of capped resolution) is exposed via the env var
///   below; it is not the default.
/// - VP8 has universal decoder support across every libwebrtc build,
///   so cross-user playback works without per-platform caveats.
///
/// `POLLIS_SCREENSHARE_CODEC` overrides the default at runtime. Accepts
/// `vp8|h264|vp9|av1|h265`; anything else (including unset) → VP8.
/// See issue #300 for the planned Preferences UI exposing this.
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
/// `None` (preference unset) defaults to 30fps. The value is clamped to the
/// 1..=60 band the picker exposes (15 = documents/browsing, 30 = standard,
/// 60 = motion/gameplay) — letting the ceiling run higher let capture race
/// ahead and tripped a tokio panic in the spike. Bitrate scales with the
/// framerate so a 60fps share isn't starved of bits while a 15fps one
/// doesn't over-spend. See #300 (the Preferences UI that sets this).
pub(super) fn resolve_screenshare_encoding(max_framerate: Option<u32>) -> (f64, u64) {
    let fps = max_framerate.unwrap_or(30).clamp(1, 60);
    let max_bitrate = fps as u64 * 130_000;
    (fps as f64, max_bitrate)
}

/// Apply `argb_to_i420` into a freshly-allocated I420 buffer at the
/// source's native dimensions. No downscale: encoders are fed the
/// full-res frame and VP8 (or whatever codec is selected) decides what
/// to do with it. Shared by all three OS push paths.
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

// ── Frame wire format (Rust -> webview) ───────────────────────────────────
//
// [ u32 LE track_key_len ][ track_key UTF-8 ]
// [ u32 LE width ][ u32 LE height ]
// [ u32 LE y_stride ][ u32 LE u_stride ][ u32 LE v_stride ]
// [ i64 LE timestamp_us ]
// [ Y plane bytes ][ U plane bytes ][ V plane bytes ]
//
// `pub(crate)` (not `pub(super)`) so the sibling `camera` module reuses it
// to mirror local webcam frames to the renderer for self-preview, exactly
// like screen share does — the two share one frame wire format + transport.
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
