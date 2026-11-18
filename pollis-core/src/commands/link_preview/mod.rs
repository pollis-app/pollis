//! Sender-generated link previews (#1987).
//!
//! The SENDER's device fetches the first link of a message it is composing and
//! sends the preview inside the end-to-end encrypted message (the `0xF9`
//! trailer, `messages::framing`). Recipients render what arrived and never
//! contact the site; the Delivery Service never fetches anything and sees only
//! ciphertext. The sender's fetch tells the site what opening the link would.
//!
//! - [`fetch_link_preview`] builds a draft for the composer: title,
//!   description and a re-encoded thumbnail, staged in memory
//!   (`commands::staging`) until the message is sent or the card is removed.
//!   `None` whenever there is no preview to show, for any reason — previews are
//!   a convenience that fails silently.
//! - The send path stores the draft with the outbox row and its thumbnail in
//!   `outbox_attachment` (flagged `link_preview`), uploads the thumbnail in the
//!   same pass as attachments, and writes the trailer ([`outgoing`]).
//! - Not built at all when the device setting is off or the overlay is strict.

pub mod fetch;
pub mod html;
pub mod wire;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

pub use wire::{LinkPreview, PreviewImage};

/// Longest edge of a thumbnail, in pixels.
pub const THUMBNAIL_MAX_EDGE: u32 = 600;
/// A re-encoded thumbnail never exceeds this many bytes.
pub const MAX_THUMBNAIL_BYTES: usize = 256 * 1024;
/// Largest source image dimension decoded.
const SOURCE_MAX_DIM: u32 = 8192;
/// Decoder allocation ceiling for a source image.
const SOURCE_MAX_ALLOC: u64 = 256 * 1024 * 1024;
const JPEG_QUALITIES: [u8; 3] = [82, 70, 55];
/// The thumbnail's name inside the encrypted object; never shown.
pub(crate) const THUMBNAIL_FILENAME: &str = "link-preview.jpg";
pub(crate) const THUMBNAIL_CONTENT_TYPE: &str = "image/jpeg";

/// `ui_state` key of the per-device "Generate link previews" setting.
const ENABLED_KEY: &str = "link_previews_enabled";

/// What the composer shows, and hands back to `send_message` / `edit_message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPreviewDraft {
    pub url: String,
    pub title: String,
    pub description: Option<String>,
    pub image: Option<DraftImage>,
}

/// A thumbnail held in staging until send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftImage {
    pub staged_id: String,
    pub width: u32,
    pub height: u32,
    /// `data:image/jpeg;base64,…` for the composer card. Ignored on the way back in.
    #[serde(default, skip_deserializing)]
    pub data_url: String,
}

/// A thumbnail ready to store with an outgoing message.
pub(crate) struct OutgoingThumbnail {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub blurhash: Option<String>,
}

/// What the send path stores for a draft: the preview (no image yet) and the
/// thumbnail bytes to upload with the message.
pub(crate) struct Outgoing {
    pub preview: LinkPreview,
    pub thumbnail: Option<OutgoingThumbnail>,
}

/// Build a preview of `url` for the composer. `None` when previews are off on
/// this device, the overlay is strict, or the page gave nothing usable.
pub async fn fetch_link_preview(url: String, state: &Arc<AppState>) -> Result<Option<LinkPreviewDraft>> {
    if crate::commands::overlay::is_strict(state) || !get_link_previews_enabled(state).await? {
        return Ok(None);
    }
    let Some(built) = build(&url, fetch::Reach::PUBLIC).await else {
        return Ok(None);
    };
    let image = match built.thumbnail {
        Some(thumb) => {
            let data_url = format!(
                "data:image/jpeg;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &thumb.jpeg)
            );
            match crate::commands::staging::stage_attachment(thumb.jpeg) {
                Ok(staged) => Some(DraftImage {
                    staged_id: staged.id,
                    width: thumb.width,
                    height: thumb.height,
                    data_url,
                }),
                Err(_) => None,
            }
        }
        None => None,
    };
    Ok(Some(LinkPreviewDraft {
        url: built.preview.url,
        title: built.preview.title,
        description: built.preview.description,
        image,
    }))
}

/// Whether this device generates previews for what it sends. On unless turned off.
pub async fn get_link_previews_enabled(state: &Arc<AppState>) -> Result<bool> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    enabled_on(db.conn())
}

/// Turn preview generation on or off for this device.
pub async fn set_link_previews_enabled(enabled: bool, state: &Arc<AppState>) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    db.conn().execute(
        "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        rusqlite::params![ENABLED_KEY, if enabled { "1" } else { "0" }],
    )?;
    Ok(())
}

fn enabled_on(conn: &rusqlite::Connection) -> Result<bool> {
    use rusqlite::OptionalExtension;
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM ui_state WHERE key = ?1",
            rusqlite::params![ENABLED_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.as_deref() != Some("0"))
}

fn not_signed_in() -> Error {
    Error::Other(anyhow::anyhow!("Not signed in"))
}

/// What [`build`] produced.
pub(crate) struct Built {
    pub preview: LinkPreview,
    pub thumbnail: Option<OutgoingThumbnail>,
}

/// Fetch `url` and build its preview under `reach`. `None` for anything that
/// is not a previewable https link with a page title.
pub(crate) async fn build(url: &str, reach: fetch::Reach) -> Option<Built> {
    if !reach.permits_start(url) {
        return None;
    }
    let start = url::Url::parse(url).ok()?;
    let deadline = tokio::time::Instant::now() + fetch::DEADLINE;
    let page = fetch::fetch(&start, fetch::Want::Html, reach, deadline).await.ok()?;
    let html = String::from_utf8_lossy(&page.body);
    let meta = html::parse(&html);
    let title = wire::clean_text(meta.title.as_deref()?, wire::MAX_TITLE_CHARS);
    if title.is_empty() {
        return None;
    }
    let description = meta
        .description
        .as_deref()
        .map(|d| wire::clean_text(d, wire::MAX_DESCRIPTION_CHARS))
        .filter(|d| !d.is_empty());
    let thumbnail = match meta.image.as_deref().and_then(|src| page.url.join(src.trim()).ok()) {
        Some(image_url) => match fetch::fetch(&image_url, fetch::Want::Image, reach, deadline).await {
            Ok(image) => thumbnail(&image.body),
            Err(_) => None,
        },
        None => None,
    };
    Some(Built {
        preview: LinkPreview {
            url: url.to_string(),
            title,
            description,
            image: None,
        },
        thumbnail,
    })
}

/// Decode untrusted image bytes and re-encode them as a bounded JPEG: at most
/// [`THUMBNAIL_MAX_EDGE`] on the long edge, under [`MAX_THUMBNAIL_BYTES`], with
/// no metadata (a fresh encode carries none).
pub(crate) fn thumbnail(bytes: &[u8]) -> Option<OutgoingThumbnail> {
    let img = crate::commands::avatar::decode_upright_limited(
        bytes,
        SOURCE_MAX_DIM,
        SOURCE_MAX_ALLOC,
        "unsupported image format",
    )
    .ok()?;
    // Shrink only: `thumbnail` would also scale a small image up to the bound.
    let bounded = if img.width() > THUMBNAIL_MAX_EDGE || img.height() > THUMBNAIL_MAX_EDGE {
        img.thumbnail(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE)
    } else {
        img
    };
    // JPEG has no alpha; flatten onto RGB.
    let small = image::DynamicImage::ImageRgb8(bounded.to_rgb8());
    let blurhash = crate::commands::r2::blurhash_of(&small).ok();
    let jpeg = crate::commands::poster::jpeg_within(&small, &JPEG_QUALITIES, MAX_THUMBNAIL_BYTES).ok()??;
    Some(OutgoingThumbnail {
        jpeg,
        width: small.width(),
        height: small.height(),
        blurhash,
    })
}

/// Turn a draft from the renderer into what the send path stores, or `None`
/// when it is not a preview of a link in `body`. The draft is renderer input,
/// so it is held to the same rules as a received preview, and its staged
/// thumbnail is re-encoded rather than trusted.
pub(crate) fn outgoing(draft: &LinkPreviewDraft, body: &str) -> Option<Outgoing> {
    let candidate = LinkPreview {
        url: draft.url.clone(),
        title: draft.title.clone(),
        description: draft.description.clone(),
        image: None,
    };
    let preview = wire::from_stored(&wire::to_stored(&candidate), body)?;
    let thumbnail = draft.image.as_ref().and_then(|img| {
        let staged = crate::commands::staging::peek_staged(&img.staged_id)?;
        thumbnail(&staged)
    });
    Some(Outgoing { preview, thumbnail })
}

/// Release a draft's staged thumbnail once the send path holds its own copy.
pub(crate) fn release_draft(draft: &LinkPreviewDraft) {
    if let Some(img) = &draft.image {
        crate::commands::staging::discard_staged(&img.staged_id);
    }
}

#[cfg(test)]
mod tests;
