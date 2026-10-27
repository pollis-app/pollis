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

// ── Clearing read notifications (#1508) ──────────────────────────────────────
//
// A notification stayed in the phone's tray after its message was read, unless
// it was the one the user tapped: opening the conversation from the list, from
// Search, from a link, or reading it on desktop or another phone left it there.
//
// Every client marks a conversation read through `mark_conversation_read`, so
// that is where the report starts: when the cursor actually MOVES (there was
// something from somebody else to read), the conversation is queued here and,
// after a short debounce, sent to `POST /v1/push/dismiss`. The DS clears the
// account's notifications for it on every device with a silent push. Desktop
// and the terminal client register no push token, but their user's phone does,
// which is the point: read on the desktop, the phone's tray clears.
//
// What the DS learns from the report — that this user read this conversation
// at about this time — is what the read-receipt envelope already tells it
// (#892, on by default). The encrypted read-cursor blob stays opaque; this is a
// separate, minimal message, sent only when the cursor moved.

/// How long cursor advances are gathered into one report. Opening three
/// conversations in a row is one request; a conversation that keeps advancing
/// while it is open (messages arriving as you read) is one request per window.
const DISMISS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Conversations read but not yet reported, per signed-in account, with when
/// each cursor last moved. Keyed by user rather than process-global like the
/// cursor push, because the report is signed with that account's `AppState`
/// and the integration suite runs several accounts in one process.
struct PendingDismiss {
    state: Arc<AppState>,
    read_at: std::collections::HashMap<String, std::time::Instant>,
    /// A debounced report is waiting to send `read_at`.
    scheduled: bool,
}

static DISMISS_PENDING: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, PendingDismiss>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// The report for a set of reads, at `now`.
///
/// Ages are whole seconds rounded UP, plus one second for the request's own
/// trip: the DS subtracts the age from its clock and clears only what it minted
/// before that, so erring old can only keep a notification, never drop one for
/// a message that arrived after the read. The newest reads win when there are
/// more than the DS accepts in one request.
pub(crate) fn dismiss_entries(
    read_at: &std::collections::HashMap<String, std::time::Instant>,
    now: std::time::Instant,
) -> Vec<pollis_api::devices::DismissEntry> {
    let mut entries: Vec<(std::time::Duration, &String)> = read_at
        .iter()
        .map(|(c, at)| (now.saturating_duration_since(*at), c))
        .collect();
    entries.sort();
    entries.truncate(pollis_api::devices::DISMISS_MAX_CONVERSATIONS);
    entries
        .into_iter()
        .map(|(age, c)| {
            let mut secs = age.as_secs();
            if age.subsec_nanos() > 0 {
                secs += 1;
            }
            pollis_api::devices::DismissEntry {
                conversation_id: c.clone(),
                read_secs_ago: u32::try_from(secs + 1).unwrap_or(u32::MAX),
            }
        })
        .collect()
}

/// Queue `conversation_id` (whose read cursor just moved) for the next
/// dismissal report, scheduling one if none is pending for this account.
///
/// Fire-and-forget, like the cursor push: a report that fails is a notification
/// that lingers on another device, never a message lost, so it must not fail
/// the call that marked the conversation read. A failed report is kept and
/// rides along with the next one — lazy recovery, no timer when nothing
/// happened.
pub(crate) fn schedule_push_dismiss(state: &Arc<AppState>, user_id: &str, conversation_id: &str) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let schedule = {
        let mut pending = DISMISS_PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let p = pending
            .entry(user_id.to_string())
            .or_insert_with(|| PendingDismiss {
                state: Arc::clone(state),
                read_at: std::collections::HashMap::new(),
                scheduled: false,
            });
        p.state = Arc::clone(state);
        p.read_at
            .insert(conversation_id.to_string(), std::time::Instant::now());
        // Entries left behind by a failed report have no task waiting on them;
        // this advance is what sends them again.
        !std::mem::replace(&mut p.scheduled, true)
    };
    if !schedule {
        return;
    }
    let user_id = user_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(DISMISS_DEBOUNCE).await;
        flush_push_dismissals(&user_id).await;
    });
}

/// Send whatever this account has queued. Taken out of the queue BEFORE the
/// request, so a read that lands during it schedules the next report instead
/// of being swallowed; put back (unless superseded) if the request fails.
async fn flush_push_dismissals(user_id: &str) {
    let taken = {
        let mut pending = DISMISS_PENDING.lock().unwrap_or_else(|e| e.into_inner());
        pending.get_mut(user_id).map(|p| {
            p.scheduled = false;
            (Arc::clone(&p.state), std::mem::take(&mut p.read_at))
        })
    };
    let Some((state, read_at)) = taken else {
        return;
    };
    if read_at.is_empty() {
        return;
    }
    let body = pollis_api::devices::DismissPushBody {
        user_id: user_id.to_string(),
        conversations: dismiss_entries(&read_at, std::time::Instant::now()),
    };
    let sent: Result<pollis_api::devices::DismissPushResponse> =
        crate::commands::mls::ds_client::ds_post_json(&state, &body).await;
    if let Err(e) = sent {
        crate::diag!("[push] read-notification dismiss failed (kept for the next read): {e}");
        let mut pending = DISMISS_PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = pending.get_mut(user_id) {
            for (conversation, at) in read_at {
                p.read_at.entry(conversation).or_insert(at);
            }
            // Bounded: a DS that never accepts (one predating #1508 answers
            // 404) cannot grow this past one request's worth.
            if p.read_at.len() > pollis_api::devices::DISMISS_MAX_CONVERSATIONS {
                let keep: std::collections::HashSet<String> =
                    dismiss_entries(&p.read_at, std::time::Instant::now())
                        .into_iter()
                        .map(|e| e.conversation_id)
                        .collect();
                p.read_at.retain(|c, _| keep.contains(c));
            }
        }
    }
}

/// Which of these tray notifications were read since, on any device (#1508)?
///
/// The phone asks on foreground and unlock with the handles still in its tray,
/// so a notification whose silent dismissal never arrived (iOS does not wake a
/// force-quit app; Android may hold a normal-priority push in Doze) clears as
/// soon as the user is back. A handle that is not this account's is simply
/// absent from the answer.
pub async fn dismissed_push_handles(
    user_id: String,
    handles: Vec<String>,
    state: &Arc<AppState>,
) -> Result<Vec<String>> {
    let mut handles: Vec<String> = handles.into_iter().filter(|h| !h.is_empty()).collect();
    handles.sort();
    handles.dedup();
    handles.truncate(pollis_api::devices::DISMISSED_QUERY_MAX_HANDLES);
    if handles.is_empty() {
        return Ok(Vec::new());
    }
    let body = pollis_api::devices::DismissedPushHandlesBody { user_id, handles };
    let resp: pollis_api::devices::DismissedPushHandlesResponse =
        crate::commands::mls::ds_client::ds_post_json(state, &body).await?;
    Ok(resp.dismissed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// Ages round up and carry a second of margin, so the DS's cutoff can only
    /// fall before the read: a message that arrived just after it keeps its
    /// notification.
    #[test]
    fn report_ages_err_old_never_young() {
        let now = Instant::now();
        let mut read_at = HashMap::new();
        read_at.insert("just-now".to_string(), now);
        read_at.insert("a-bit-ago".to_string(), now - Duration::from_millis(1500));
        read_at.insert("exactly-3s".to_string(), now - Duration::from_secs(3));
        let mut entries = dismiss_entries(&read_at, now);
        entries.sort_by(|a, b| a.conversation_id.cmp(&b.conversation_id));
        let ages: Vec<(&str, u32)> = entries
            .iter()
            .map(|e| (e.conversation_id.as_str(), e.read_secs_ago))
            .collect();
        assert_eq!(ages, vec![("a-bit-ago", 3), ("exactly-3s", 4), ("just-now", 1)]);
    }

    /// More reads than one request carries: the newest are kept.
    #[test]
    fn a_long_backlog_keeps_the_newest_reads() {
        let now = Instant::now();
        let max = pollis_api::devices::DISMISS_MAX_CONVERSATIONS;
        let read_at: HashMap<String, Instant> = (0..max + 10)
            .map(|i| (format!("c{i:03}"), now - Duration::from_secs(i as u64)))
            .collect();
        let entries = dismiss_entries(&read_at, now);
        assert_eq!(entries.len(), max);
        assert!(entries.iter().all(|e| e.read_secs_ago <= max as u32 + 1));
        assert!(!entries.iter().any(|e| e.conversation_id == format!("c{:03}", max + 9)));
    }

    /// The report body is exactly what the DS expects on the wire.
    #[test]
    fn the_report_serializes_to_the_wire_shape() {
        let body = pollis_api::devices::DismissPushBody {
            user_id: "u1".into(),
            conversations: vec![pollis_api::devices::DismissEntry {
                conversation_id: "c1".into(),
                read_secs_ago: 2,
            }],
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "user_id": "u1",
                "conversations": [{ "conversation_id": "c1", "read_secs_ago": 2 }],
            })
        );
    }
}
