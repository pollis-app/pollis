//! Custom per-group emoji (#848): creation, removal, and the unreferenced-object
//! sweep.
//!
//! Wire types only; see `pollis-delivery` for handling.

use serde::{Deserialize, Serialize};

/// Shortest permitted shortcode.
pub const SHORTCODE_MIN: usize = 2;
/// Longest permitted shortcode.
pub const SHORTCODE_MAX: usize = 32;

/// `[a-z0-9_]{2,32}`, hand-rolled (no regex dependency). The one definition:
/// the DS enforces it, the apps pre-check with it, and `pollis-import`
/// normalises foreign emoji names to it.
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(SHORTCODE_MIN..=SHORTCODE_MAX).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[derive(Serialize, Deserialize)]
pub struct CreateEmojiBody {
    pub group_id: String,
    pub shortcode: String,
    /// SHA-256 (lowercase hex) of the RE-ENCODED bytes.
    pub content_hash: String,
    /// `image/webp` or `image/gif`. Allowlisted, not sniffed and not trusted as
    /// "whatever the file said it was".
    pub content_type: String,
    /// Size of the stored object. Bounded by `pollis_delivery::emoji::EMOJI_MAX_BYTES` and bound into
    /// the presign signature, so a lie here cannot become a large R2 object.
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

/// `POST /v1/emoji/gc` — sweep custom-emoji objects no group still references.
///
/// No parameters; the empty struct gives the endpoint a row in [`crate::ENDPOINTS`].
#[derive(Serialize, Deserialize)]
pub struct EmojiGcBody {}

/// `POST /v1/emoji/gc` — what the sweep collected.
///
/// The caller deletes the R2 blobs behind `r2_keys`, or they are stranded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiGcResponse {
    /// How many objects were collected — `r2_keys.len()`, sent explicitly
    /// because it is what an operator reads out of a `curl`.
    pub collected: usize,
    /// The R2 keys whose rows are now gone. Empty when nothing was unreferenced.
    #[serde(default)]
    pub r2_keys: Vec<String>,
}
