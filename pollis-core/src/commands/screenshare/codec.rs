//! Codec choice, I420 conversion, publish limits and frame-bytes packing, shared by push and drain.

use libwebrtc::video_frame::{I420Buffer, VideoBuffer};
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

/// What the publish will carry, applied as far upstream as each capture backend allows
/// (PipeWire/SCK/WGC hints, X11 tick) with the parent's scale + pacer as the backstop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShareLimits {
    /// The encoder's `max_framerate`; frames past it are dropped by libwebrtc anyway.
    pub max_fps: u32,
    /// Tallest frame to publish; `None` = the source's own size.
    pub max_height: Option<u32>,
}

impl ShareLimits {
    /// From the renderer's two preferences: fps clamped as the encoder clamps it, a
    /// resolution of `0` (or absent) meaning "source".
    pub fn resolve(max_framerate: Option<u32>, max_height: Option<u32>) -> Self {
        let (fps, _) = resolve_screenshare_encoding(max_framerate);
        Self {
            max_fps: fps as u32,
            max_height: max_height.filter(|h| *h > 0),
        }
    }

    /// The even `(w, h)` a `(w, h)` source should be captured or scaled to, or `None`
    /// when it already fits.
    pub fn fit(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let max_height = self.max_height?;
        if height <= max_height || height == 0 {
            return None;
        }
        let w = ((width as u64 * max_height as u64 + height as u64 / 2) / height as u64) as u32;
        Some(((w.max(2)) & !1, (max_height.max(2)) & !1))
    }
}

/// Skip frames the encoder's own frame-rate adapter would drop, before paying for the
/// conversion: admits at most `fps` per second, measured on the wall clock.
pub(crate) struct FramePacer {
    interval: std::time::Duration,
    last: Option<std::time::Instant>,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        // A hair under the period: a steady source at exactly `fps` must not be halved by
        // scheduler jitter.
        let micros = 950_000 / fps.max(1) as u64;
        Self {
            interval: std::time::Duration::from_micros(micros),
            last: None,
        }
    }

    pub fn admit(&mut self, now: std::time::Instant) -> bool {
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < self.interval)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// `argb_to_i420` into a fresh I420 buffer at native size (no downscale).
pub(crate) fn convert_to_i420(width: i32, height: i32, src_stride: u32, argb: &[u8]) -> I420Buffer {
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

/// The parent-side backstop for a capture backend that could not size itself: libyuv
/// I420 scale down to the limit's height, aspect kept.
pub(crate) fn bound_to_limits(mut buffer: I420Buffer, limits: &ShareLimits) -> I420Buffer {
    match limits.fit(buffer.width(), buffer.height()) {
        Some((w, h)) => buffer.scale(w as i32, h as i32),
        None => buffer,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_resolve_like_the_encoder_and_treat_zero_as_source() {
        let l = ShareLimits::resolve(None, None);
        assert_eq!(
            l,
            ShareLimits {
                max_fps: 30,
                max_height: None
            }
        );
        let l = ShareLimits::resolve(Some(144), Some(0));
        assert_eq!(
            l,
            ShareLimits {
                max_fps: 60,
                max_height: None
            }
        );
        let l = ShareLimits::resolve(Some(15), Some(720));
        assert_eq!(
            l,
            ShareLimits {
                max_fps: 15,
                max_height: Some(720)
            }
        );
    }

    #[test]
    fn fit_scales_only_what_is_taller_than_the_limit_and_keeps_aspect_even() {
        let l = ShareLimits {
            max_fps: 30,
            max_height: Some(1080),
        };
        assert_eq!(l.fit(1920, 1080), None);
        assert_eq!(l.fit(1280, 720), None);
        assert_eq!(l.fit(3840, 2160), Some((1920, 1080)));
        // 5K retina (16:10): 5120x2880 → 1920x1080 would break aspect; width follows.
        assert_eq!(l.fit(5120, 2880), Some((1920, 1080)));
        assert_eq!(l.fit(2560, 1600), Some((1728, 1080)));
        // Odd results round down to even.
        assert_eq!(l.fit(1366, 1300), Some((1134, 1080)));
        let source = ShareLimits {
            max_fps: 30,
            max_height: None,
        };
        assert_eq!(source.fit(7680, 4320), None);
    }

    #[test]
    fn pacer_admits_at_most_fps_per_second() {
        let mut p = FramePacer::new(30);
        let t0 = std::time::Instant::now();
        let step = std::time::Duration::from_micros(1_000_000 / 60);
        let admitted = (0..120).filter(|i| p.admit(t0 + step * *i)).count();
        // 2 s of a 60 Hz source through a 30 fps pacer: ~60 frames, never the full 120.
        assert!((55..=62).contains(&admitted), "{admitted}");
        // A source already at the cap passes whole (period jitter must not halve it).
        let mut p = FramePacer::new(30);
        let step = std::time::Duration::from_micros(1_000_000 / 30);
        let admitted = (0..60).filter(|i| p.admit(t0 + step * *i)).count();
        assert_eq!(admitted, 60);
    }
}
