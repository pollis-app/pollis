//! Push-token registration (#344) — **always compiled, all targets**.
//!
//! Fan-out is server-side (#987); payloads carry only an opaque handle (#1122).

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// Upsert a device's Expo push token, keyed on the token so re-registering reassigns it.
///
/// `caps` (#1599): what this build can do with a push, e.g. `["rich"]`. Sent on every
/// register, since the DS replaces the stored set; empty = today's payload.
pub async fn register_push_token(
    user_id: String,
    token: String,
    platform: String,
    caps: Vec<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    let body = pollis_api::devices::PushTokenBody {
        token,
        platform,
        updated_at: now,
        user_id: Some(user_id),
        caps,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    Ok(())
}

/// Trade an opaque push handle for the conversation it was minted for (#1122).
///
/// `None`: unknown, expired or not this user's, indistinguishable on purpose. Cached, not
/// single-use: a background push and a later tap resolve the same handle.
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
/// Persisting them would re-create locally what #1122 removed from the wire.
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
    // A full cache is dropped; every entry is re-resolvable, so the cost is one round trip.
    if map.len() >= HANDLE_CACHE_MAX {
        map.clear();
    }
    map.insert(
        handle.to_string(),
        (conversation_id.to_string(), kind.to_string()),
    );
}

// Clearing read notifications (#1508). The DS learns only what read receipts tell it (#892).

/// How long cursor advances are gathered into one report.
const DISMISS_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Conversations read but not yet reported, keyed per account (reports are signed per account).
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
/// Ages round UP plus a second, so the DS errs toward keeping a notification; newest win.
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
/// Fire-and-forget: a failed report is kept and rides along with the next one.
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
        // Leftovers from a failed report have no task; this advance resends them.
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
            // Bounded: a DS predating #1508 answers 404 forever.
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
/// Catches silent dismissals that never arrived (iOS force-quit, Android Doze).
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
