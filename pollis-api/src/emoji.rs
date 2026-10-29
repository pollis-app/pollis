//! Custom per-group emoji (#848): creation, removal, and the unreferenced-object
//! sweep. Wire types only.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct CreateEmojiBody {
    pub group_id: String,
    pub shortcode: String,
    /// SHA-256 (lowercase hex) of the RE-ENCODED bytes.
    pub content_hash: String,
    /// `image/webp` or `image/gif`, allowlisted rather than trusted.
    pub content_type: String,
    /// Size of the stored object, bounded by `EMOJI_MAX_BYTES` and signed into the
    /// presign, so a lie cannot become a large R2 object.
    pub size_bytes: u64,
    #[serde(default)]
    pub animated: bool,
    /// No-auth path only; ignored when auth is enforced.
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveEmojiBody {
    pub group_id: String,
    pub shortcode: String,
    /// No-auth path only; ignored when auth is enforced.
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// `POST /v1/emoji/gc`: sweep emoji objects no group references. No
/// parameters: scope and decision come from the reference rows alone.
#[derive(Serialize, Deserialize)]
pub struct EmojiGcBody {}

/// `POST /v1/emoji/gc`: what the sweep collected. The caller deletes the R2
/// blobs behind `r2_keys`, since the dropped rows were their only record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiGcResponse {
    /// How many objects were collected (`r2_keys.len()`, explicit for operators).
    pub collected: usize,
    /// The R2 keys whose rows are gone.
    #[serde(default)]
    pub r2_keys: Vec<String>,
}
