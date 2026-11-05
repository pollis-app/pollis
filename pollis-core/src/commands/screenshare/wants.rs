//! Subscriber-driven video: what the renderer's tiles actually want from each track.
//!
//! A remote track used to be decoded, packed and shipped at the publisher's full resolution
//! and rate whether it drew on a 320 px preview or nowhere at all. The tile now reports its
//! on-screen size and visibility; that drives the SFU layer (`update_video_dimensions`),
//! pauses the subscription for a hidden tile (`set_enabled(false)`), and bounds what the
//! drain task packs onto the frame socket. The two local self-preview keys ride the same
//! map so the capture paths can scale and pace their mirror the same way.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::{error::Result, state::AppState};

/// What one or more tiles need from a track: the largest on-screen size in device pixels,
/// and the fastest paint rate any of them will use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoWants {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// A track's demand as the drain task sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WantState {
    /// No tile has reported yet: deliver at native size so the first frame (and with it the
    /// `RemoteStarted` the tile is created from) can happen at all.
    Unknown,
    Wanted(VideoWants),
    /// Every tile is gone or off-screen: nothing is packed, and the SFU is told to pause.
    Hidden,
}

impl WantState {
    /// Frames to skip entirely: nobody would paint them.
    pub fn is_hidden(self) -> bool {
        matches!(self, WantState::Hidden)
    }

    /// The even target size a decoded `(w, h)` frame should be packed at, or `None` to pack
    /// it as decoded. Only a drop of at least 2× on both axes is worth the scale: below
    /// that the SFU layer is already close, and the bytes saved don't pay for the pass.
    pub fn pack_size(self, decoded: (u32, u32)) -> Option<(u32, u32)> {
        let WantState::Wanted(w) = self else {
            return None;
        };
        let (dw, dh) = decoded;
        if w.width == 0 || w.height == 0 || dw < w.width * 2 || dh < w.height * 2 {
            return None;
        }
        // Fit inside the wanted box, keeping the source aspect.
        let scale = f64::min(w.width as f64 / dw as f64, w.height as f64 / dh as f64);
        let tw = ((dw as f64 * scale).round() as u32).max(2) & !1;
        let th = ((dh as f64 * scale).round() as u32).max(2) & !1;
        Some((tw, th))
    }

    /// The pacing interval, or `None` for "every frame".
    pub fn min_interval(self) -> Option<std::time::Duration> {
        match self {
            WantState::Wanted(VideoWants { fps, .. }) if fps > 0 => {
                // Slightly under the period, so a steady source at exactly `fps` isn't halved
                // by timer jitter.
                Some(std::time::Duration::from_micros(900_000 / fps as u64))
            }
            _ => None,
        }
    }
}

pub type WantsTx = watch::Sender<WantState>;
pub type WantsRx = watch::Receiver<WantState>;

/// The watch for `track_key`, created on first use so a tile that reports before the drain
/// task registers is not lost.
pub(super) fn wants_channel(ss: &mut super::ScreenShareState, track_key: &str) -> WantsRx {
    if let Some(tx) = ss.video_wants.get(track_key) {
        return tx.subscribe();
    }
    let (tx, rx) = watch::channel(WantState::Unknown);
    ss.video_wants.insert(track_key.to_string(), tx);
    rx
}

/// Renderer → core: a tile's current demand for `track_key`, `None` when nothing shows it.
///
/// Remote tracks also carry it to the SFU: a hidden track is paused rather than decoded
/// into nothing, a small tile asks for a small simulcast layer.
pub async fn set_video_wants(
    state: &Arc<AppState>,
    track_key: String,
    wants: Option<VideoWants>,
) -> Result<()> {
    let next = match wants {
        Some(w) => WantState::Wanted(w),
        None => WantState::Hidden,
    };
    let publication = {
        let mut ss = state.screenshare.lock().await;
        let _ = wants_channel(&mut ss, &track_key);
        if let Some(tx) = ss.video_wants.get(&track_key) {
            tx.send_replace(next);
        }
        ss.remote_publications.get(&track_key).cloned()
    };
    let Some(publication) = publication else {
        return Ok(());
    };
    match wants {
        None => {
            crate::diag!("[screenshare] {track_key}: hidden, pausing subscription");
            publication.set_enabled(false);
        }
        Some(w) => {
            if !publication.is_enabled() {
                publication.set_enabled(true);
            }
            // Dimensions, not a quality tier: the SFU maps them onto whatever layers the
            // publisher offers, which keeps a non-simulcast track from logging a warning.
            publication.update_video_dimensions(livekit::track::TrackDimension(w.width, w.height));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(width: u32, height: u32, fps: u32) -> WantState {
        WantState::Wanted(VideoWants { width, height, fps })
    }

    #[test]
    fn packs_as_decoded_until_the_drop_is_at_least_twofold() {
        assert_eq!(WantState::Unknown.pack_size((1920, 1080)), None);
        assert_eq!(wanted(1280, 720, 30).pack_size((1920, 1080)), None);
        assert_eq!(
            wanted(960, 540, 30).pack_size((1920, 1080)),
            Some((960, 540))
        );
        assert_eq!(
            wanted(320, 180, 15).pack_size((1920, 1080)),
            Some((320, 180))
        );
    }

    #[test]
    fn pack_size_keeps_aspect_and_even_dimensions() {
        // A square tile showing a 16:9 frame: width-bound, height follows the source (225 → 224).
        assert_eq!(
            wanted(400, 400, 15).pack_size((1920, 1080)),
            Some((400, 224))
        );
        // A wide, short tile: height-bound (533 → 532).
        assert_eq!(
            wanted(900, 300, 15).pack_size((1920, 1080)),
            Some((532, 300))
        );
        // One axis under a twofold drop: the SFU layer is close enough, pack as decoded.
        assert_eq!(wanted(2000, 300, 15).pack_size((1920, 1080)), None);
        // A zero want is "unknown size", not a zero-size frame.
        assert_eq!(wanted(0, 0, 15).pack_size((1920, 1080)), None);
    }

    #[test]
    fn hidden_skips_everything_and_fps_paces() {
        assert!(WantState::Hidden.is_hidden());
        assert!(!wanted(100, 100, 15).is_hidden());
        assert_eq!(WantState::Unknown.min_interval(), None);
        assert_eq!(
            wanted(100, 100, 15).min_interval(),
            Some(std::time::Duration::from_micros(60_000))
        );
        assert_eq!(wanted(100, 100, 0).min_interval(), None);
    }
}
