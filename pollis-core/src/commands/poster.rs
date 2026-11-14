//! A video attachment's poster (#1837): a still frame shown before the video is
//! played, carried inside the encrypted `_att` envelope as `pv` beside `bh`.
//!
//! The sending client decodes the frame with its platform decoder (the webview
//! on desktop, AVFoundation / MediaMetadataRetriever through expo-video on
//! mobile) and hands it here as base64. This module bounds it: one small JPEG,
//! re-encoded until it fits [`POSTER_MAX_JPEG_BYTES`], plus a blurhash and the
//! frame's dimensions. Receivers on older builds ignore `pv`; receivers without
//! one fall back to the blurhash, a local decode or a placeholder.
//!
//! A local decode is bounded here too and saved in the local `video_poster_cache`
//! table (#1894), so a video from an older sender is fetched and decoded once.

use std::sync::Arc;

use base64::Engine as _;
use image::{DynamicImage, GenericImageView};
use rusqlite::{Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Longest edge of the poster that travels in the envelope.
pub const POSTER_MAX_EDGE: u32 = 320;
/// The most one poster's JPEG may hold; its base64 is 4/3 of this.
pub const POSTER_MAX_JPEG_BYTES: usize = 12 * 1024;
/// The most base64 poster text one message may carry, all attachments together:
/// the envelope must stay well inside the 256 KiB a pinned or Vault blob, a
/// history transfer row and a pending edit allow.
pub const POSTER_ENVELOPE_BUDGET: usize = 96 * 1024;
/// The largest frame a client may hand over, as base64 text.
pub const POSTER_FRAME_MAX_B64: usize = 8 * 1024 * 1024;

/// Qualities tried in order until the JPEG fits, then the edge is halved.
const QUALITIES: [u8; 4] = [72, 60, 48, 36];
const MIN_EDGE: u32 = 80;

/// A poster's JPEG as standard base64, never over [`POSTER_MAX_JPEG_BYTES`]
/// decoded: [`encode_poster`] is the only constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosterJpeg(String);

impl PosterJpeg {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// What a frame yields for the envelope.
#[derive(Debug, Clone)]
pub struct VideoPoster {
    pub blurhash: String,
    /// The frame's dimensions, which carry the video's displayed aspect ratio.
    pub width: u32,
    pub height: u32,
    /// `None` when no JPEG small enough could be made; the blurhash still travels.
    pub jpeg: Option<PosterJpeg>,
}

/// Build the poster from a client's frame: base64 of a JPEG/PNG/WebP, with or
/// without a `data:` prefix.
pub fn from_frame_b64(frame: &str) -> anyhow::Result<VideoPoster> {
    let b64 = frame
        .split_once(";base64,")
        .map(|(_, rest)| rest)
        .unwrap_or(frame)
        .trim();
    if b64.len() > POSTER_FRAME_MAX_B64 {
        anyhow::bail!("poster frame is {} base64 bytes; the limit is {POSTER_FRAME_MAX_B64}", b64.len());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("poster frame base64: {e}"))?;
    from_frame(&bytes)
}

/// [`from_frame_b64`] on raw image bytes.
pub fn from_frame(bytes: &[u8]) -> anyhow::Result<VideoPoster> {
    let img = image::load_from_memory(bytes).map_err(|e| anyhow::anyhow!("poster decode: {e}"))?;
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        anyhow::bail!("poster frame is empty");
    }
    let blurhash = crate::commands::r2::blurhash_of(&img)?;
    Ok(VideoPoster {
        blurhash,
        width,
        height,
        jpeg: encode_poster(&img),
    })
}

/// The smallest-effort JPEG under the cap: lower quality first, then a smaller edge.
fn encode_poster(img: &DynamicImage) -> Option<PosterJpeg> {
    let mut edge = POSTER_MAX_EDGE;
    while edge >= MIN_EDGE {
        // JPEG has no alpha; flatten onto RGB.
        let small = DynamicImage::ImageRgb8(img.thumbnail(edge, edge).to_rgb8());
        for quality in QUALITIES {
            let mut out = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            if small.write_with_encoder(encoder).is_err() {
                return None;
            }
            if out.len() <= POSTER_MAX_JPEG_BYTES {
                return Some(PosterJpeg(base64::engine::general_purpose::STANDARD.encode(out)));
            }
        }
        edge /= 2;
    }
    None
}

/// Keep posters in order until the message's [`POSTER_ENVELOPE_BUDGET`] is spent;
/// a poster past it is dropped (its blurhash stays).
pub fn within_budget<'a>(posters: impl IntoIterator<Item = &'a mut Option<String>>) {
    let mut spent = 0usize;
    for slot in posters {
        let Some(p) = slot.as_ref() else {
            continue;
        };
        if spent + p.len() > POSTER_ENVELOPE_BUDGET {
            *slot = None;
        } else {
            spent += p.len();
        }
    }
}

/// A poster this device decoded for a video whose sender sent no `pv` (#1894),
/// as saved in the local `video_poster_cache` table and handed to the clients.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedVideoPoster {
    /// The bounded JPEG as base64, or `None` when none fit (the blurhash stands in).
    pub jpeg: Option<String>,
    pub blurhash: String,
    pub width: u32,
    pub height: u32,
}

impl From<VideoPoster> for SavedVideoPoster {
    fn from(p: VideoPoster) -> Self {
        Self {
            jpeg: p.jpeg.map(PosterJpeg::into_string),
            blurhash: p.blurhash,
            width: p.width,
            height: p.height,
        }
    }
}

/// The saved poster for `content_hash`, if this device made one.
pub fn saved_poster(conn: &Connection, content_hash: &str) -> Result<Option<SavedVideoPoster>> {
    Ok(conn
        .query_row(
            "SELECT jpeg, blurhash, width, height FROM video_poster_cache WHERE content_hash = ?1",
            rusqlite::params![content_hash],
            |row| {
                Ok(SavedVideoPoster {
                    jpeg: row.get(0)?,
                    blurhash: row.get(1)?,
                    width: row.get(2)?,
                    height: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Save `poster` for `content_hash`, replacing any earlier one.
pub fn save_poster(conn: &Connection, content_hash: &str, poster: &SavedVideoPoster) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO video_poster_cache (content_hash, jpeg, blurhash, width, height)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            content_hash,
            poster.jpeg,
            poster.blurhash,
            poster.width,
            poster.height
        ],
    )?;
    Ok(())
}

/// The poster this device saved for a video with no `pv` (#1894); `None` before the first decode.
pub async fn get_local_video_poster(
    content_hash: String,
    state: &Arc<AppState>,
) -> Result<Option<SavedVideoPoster>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    saved_poster(db.conn(), &content_hash)
}

/// Bound a frame the client decoded from a video with no `pv` and save it locally (#1894),
/// so later renders read it instead of fetching and decoding the video again. Returns what
/// was saved. Never sent anywhere.
pub async fn save_local_video_poster(
    content_hash: String,
    frame: String,
    state: &Arc<AppState>,
) -> Result<SavedVideoPoster> {
    let poster: SavedVideoPoster = tokio::task::spawn_blocking(move || from_frame_b64(&frame))
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("poster task failed: {e}")))??
        .into();
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    save_poster(db.conn(), &content_hash, &poster)?;
    Ok(poster)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, noisy: bool) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            if noisy {
                // Hash-like noise defeats JPEG compression.
                let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).wrapping_mul(97);
                image::Rgb([(v >> 3) as u8, (v >> 11) as u8, (v >> 19) as u8])
            } else {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
            }
        });
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encode");
        bytes
    }

    #[test]
    fn a_frame_becomes_a_small_jpeg_with_its_aspect_and_a_blurhash() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(frame(1280, 720, false));
        let poster = from_frame_b64(&format!("data:image/png;base64,{b64}")).expect("poster");
        assert_eq!((poster.width, poster.height), (1280, 720));
        assert_eq!(poster.blurhash.len(), 1 + 1 + 4 + 2 * 11);
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(poster.jpeg.expect("a jpeg").as_str())
            .unwrap();
        assert!(jpeg.len() <= POSTER_MAX_JPEG_BYTES);
        let decoded = image::load_from_memory(&jpeg).expect("a real jpeg");
        assert_eq!(decoded.dimensions(), (POSTER_MAX_EDGE, 180));
    }

    /// The cap holds however incompressible the frame is: the JPEG shrinks or is dropped.
    #[test]
    fn no_poster_ever_exceeds_the_cap() {
        let poster = from_frame(&frame(1920, 1080, true)).expect("poster");
        if let Some(jpeg) = poster.jpeg {
            let bytes = base64::engine::general_purpose::STANDARD.decode(jpeg.as_str()).unwrap();
            assert!(bytes.len() <= POSTER_MAX_JPEG_BYTES, "{} bytes", bytes.len());
        }
    }

    #[test]
    fn garbage_is_an_error_not_a_poster() {
        assert!(from_frame_b64("not base64!").is_err());
        assert!(from_frame(b"not an image").is_err());
        let huge = "A".repeat(POSTER_FRAME_MAX_B64 + 4);
        assert!(from_frame_b64(&huge).is_err());
    }

    #[test]
    fn the_budget_keeps_posters_in_order_until_spent() {
        let big = "A".repeat(POSTER_ENVELOPE_BUDGET / 2);
        let mut posters = [
            Some(big.clone()),
            None,
            Some(big.clone()),
            Some(big.clone()),
            Some("AAAA".to_string()),
        ];
        within_budget(posters.iter_mut());
        assert!(posters[0].is_some());
        assert!(posters[2].is_some());
        assert!(posters[3].is_none(), "over budget");
        assert!(posters[4].is_none(), "the budget is spent exactly");
    }

    const HASH: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";

    fn saved_frame() -> SavedVideoPoster {
        from_frame(&frame(640, 360, false)).expect("poster").into()
    }

    /// #1894: the second render reads what the first decode saved.
    #[test]
    fn a_saved_poster_reads_back() {
        let db = crate::db::local::LocalDb::open_in_memory().expect("db");
        let conn = db.conn();
        assert_eq!(saved_poster(conn, HASH).unwrap(), None, "nothing before the first decode");

        let poster = saved_frame();
        assert!(poster.jpeg.is_some());
        save_poster(conn, HASH, &poster).expect("save");
        assert_eq!(saved_poster(conn, HASH).unwrap(), Some(poster.clone()));
        assert_eq!(saved_poster(conn, "another-video").unwrap(), None);

        // A second save (two tiles racing) replaces, never duplicates.
        save_poster(conn, HASH, &poster).expect("save again");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM video_poster_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// The table holds only what `from_frame` can produce: a bounded JPEG and a real size.
    #[test]
    fn the_table_refuses_an_unbounded_or_empty_poster() {
        let db = crate::db::local::LocalDb::open_in_memory().expect("db");
        let conn = db.conn();
        let too_big = SavedVideoPoster {
            jpeg: Some("A".repeat(16_388)),
            ..saved_frame()
        };
        assert!(save_poster(conn, HASH, &too_big).is_err());
        let no_size = SavedVideoPoster {
            width: 0,
            ..saved_frame()
        };
        assert!(save_poster(conn, HASH, &no_size).is_err());
        assert!(save_poster(conn, "", &saved_frame()).is_err());
        assert_eq!(saved_poster(conn, HASH).unwrap(), None);
    }

    /// Wiping the account's data (logout with delete, account deletion) and an identity
    /// reset (a fresh DB key) leave no saved poster behind.
    #[test]
    fn a_wipe_or_reset_drops_saved_posters() {
        use crate::db::local::{remove_user_database, LocalDb};
        let dir = tempfile::tempdir().expect("tempdir");
        const USER: &str = "poster-wipe";
        let key = [0x11u8; 32];

        {
            let db = LocalDb::open_for_user_in(dir.path(), USER, &key).expect("open");
            save_poster(db.conn(), HASH, &saved_frame()).expect("save");
        }
        {
            let db = LocalDb::open_for_user_in(dir.path(), USER, &key).expect("reopen");
            assert!(saved_poster(db.conn(), HASH).unwrap().is_some(), "kept across a restart");
        }

        remove_user_database(dir.path(), USER);
        {
            let db = LocalDb::open_for_user_in(dir.path(), USER, &key).expect("fresh");
            assert_eq!(saved_poster(db.conn(), HASH).unwrap(), None, "wiped");
            save_poster(db.conn(), HASH, &saved_frame()).expect("save");
        }

        let reset_key = [0x22u8; 32];
        let db = LocalDb::open_for_user_in(dir.path(), USER, &reset_key).expect("reset");
        assert_eq!(saved_poster(db.conn(), HASH).unwrap(), None, "reset");
    }
}
