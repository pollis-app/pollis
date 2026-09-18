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
//! What a push carries has never included content — no plaintext, no sender.
//! Since #1122 it does not include the conversation either: the payload carries
//! an opaque `h` handle, and [`resolve_push_handle`] trades it for
//! `{ conversation_id, kind }` over this client's own authenticated channel.
//! Expo/APNs/FCM are outside the overlay by design, so anything in the payload
//! is disclosed to three third parties on every message — and "which
//! conversation, when" is exactly the signal the metadata-minimisation design
//! sets out to withhold.
//!
//! Foreground delivery still uses the LiveKit realtime path; push is strictly
//! the background/closed path.

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

/// Trade an opaque push handle for the conversation it was minted for (#1122).
///
/// `None` means the handle is unknown, expired, swept, or not this user's — the
/// DS answers all four identically on purpose, so possessing a handle proves
/// nothing. The caller degrades to opening the app rather than the conversation.
///
/// **Cached, and deliberately not single-use.** A background data push resolves
/// the handle to re-ingest, and a later tap resolves the SAME handle to
/// navigate; burning it on first use would break tap-to-conversation for every
/// notification already ingested. The cache makes the repeat free and survives
/// the round trip being unavailable offline.
pub async fn resolve_push_handle(
    handle: String,
    state: &Arc<AppState>,
) -> Result<Option<(String, String)>> {
    if let Some(hit) = handle_cache_get(&handle).await {
        return Ok(Some(hit));
    }
    let body = pollis_api::devices::ResolvePushHandleBody {
        handle: handle.clone(),
    };
    let resp: pollis_api::devices::ResolvePushHandleResponse =
        crate::commands::mls::ds_client::ds_post_json(state, &body).await?;
    match (resp.conversation_id, resp.kind) {
        (Some(c), Some(k)) => {
            handle_cache_put(&handle, &c, &k).await;
            Ok(Some((c, k)))
        }
        _ => Ok(None),
    }
}

/// Resolved handles, in memory only.
///
/// Not persisted: a handle is worth minutes-to-days and the mapping is exactly
/// the "which conversation" fact the payload stopped carrying, so writing it to
/// disk would re-create locally what #1122 removed from the wire. A cold start
/// simply resolves again.
static HANDLE_CACHE: std::sync::LazyLock<
    tokio::sync::Mutex<std::collections::HashMap<String, (String, String)>>,
> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

/// Bound on the cache, so a stream of notifications cannot grow it without end.
const HANDLE_CACHE_MAX: usize = 256;

async fn handle_cache_get(handle: &str) -> Option<(String, String)> {
    HANDLE_CACHE.lock().await.get(handle).cloned()
}

async fn handle_cache_put(handle: &str, conversation_id: &str, kind: &str) {
    let mut map = HANDLE_CACHE.lock().await;
    // Crude but sufficient: a full cache is dropped rather than evicted
    // one-by-one. Every entry is re-resolvable over the network, so the cost of
    // being wrong here is one round trip, not a lost notification.
    if map.len() >= HANDLE_CACHE_MAX {
        map.clear();
    }
    map.insert(
        handle.to_string(),
        (conversation_id.to_string(), kind.to_string()),
    );
}
