//! Push-token registration (#344) — **always compiled, all targets**.
//!
//! A mobile client upserts its Expo push token (one row per device install)
//! through `POST /v1/push-tokens`. Desktop never registers one, so its users
//! have no rows.
//!
//! # The fan-out moved server-side (#987)
//!
//! `notify_new_message` used to live here: it read the conversation's members,
//! read their `push_token` rows, and POSTed to Expo directly. All three are now
//! the DS's (`pollis_delivery::push`), driven by the `push_to` field on
//! `POST /v1/messages/send`. Three things went with it:
//!
//!   * the client's need for a whole-database read credential to see other
//!     people's push tokens — the most identifying row it had any reason to read
//!     about someone else;
//!   * two dependent round trips on the hot send path, after the send had
//!     already landed;
//!   * every client talking to `exp.host` directly, which is a non-first-party
//!     host outside the overlay allowlist, so a relay could never carry it.
//!
//! What a push carries is unchanged: `{ conversationId, kind }` and nothing
//! else — enough for the client to route and re-ingest the (still-encrypted)
//! message locally, never the plaintext, the sender, or any content. Foreground
//! delivery still uses the LiveKit realtime path; push is strictly the
//! background/closed path.

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// Upsert a device's Expo push token. Keyed on the token (unique per device
/// install) so re-registering from the same device — e.g. after switching
/// accounts — reassigns ownership rather than creating a duplicate row.
pub async fn register_push_token(
    user_id: String,
    token: String,
    platform: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    // DS seam: route the owner-scoped upsert through the Delivery Service (the
    // write API).
    let body = pollis_api::devices::PushTokenBody {
        token,
        platform,
        updated_at: now,
        user_id: Some(user_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    Ok(())
}
