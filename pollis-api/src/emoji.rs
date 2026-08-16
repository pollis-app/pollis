//! Custom per-group emoji (#848): creation, removal, and the unreferenced-object
//! sweep.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

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
/// Carries no parameters: the sweep's scope is the whole store and its decision
/// is derived from the reference rows, never from anything the caller says. The
/// empty struct exists so the endpoint is a first-class row in the
/// [`crate::ENDPOINTS`] table (and so route-coverage tests reach it) rather than
/// a loose `json!({})` at the call site.
#[derive(Serialize, Deserialize)]
pub struct EmojiGcBody {}
