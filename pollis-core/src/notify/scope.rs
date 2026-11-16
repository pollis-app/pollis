//! One push fetch's scope (#1804): what it read in a single round trip, and the
//! follow-up work it moves off the notification's critical path.
//!
//! **Prefetch.** `POST /v1/push/fetch` answers the handle resolve, this device's
//! Welcome read, the conversation's catch-up and a DM's peer account keys at once.
//! The fetch then runs the app's own ingest unchanged inside [`run`]; the readers
//! that ingest calls ([`catch_up`], [`welcomes`], [`account_keys`]) answer from the
//! prefetch instead of the network. Envelopes are served once (a second pass goes
//! to the network, as before), and the prefetch is installed only when no Welcome is
//! pending: applying a Welcome read before the cross-process lock was taken could
//! replay one another drain already applied, so that rare case keeps the old path.
//!
//! **Tail.** A notification needs the message stored, not the watermark advanced
//! or the delivered receipt sent. Inside a scope that allows it, the ingest hands
//! that work to [`defer`], which runs it on the core's runtime after the fetch
//! returns; the host waits for it with [`settle`] (Android, inside its foreground
//! service) before letting the process go.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pollis_api::account_reads::AccountKeyRow;
use pollis_api::devices::PushFetchResponse;
use pollis_api::directory::CatchUpResponse;
use pollis_api::reads::FetchWelcomesResponse;

tokio::task_local! {
    static SCOPE: Arc<Scope>;
    /// Set while deferred work runs, so what it spawns can be waited for too ([`spawn_in_tail`]).
    static IN_TAIL: ();
}

/// Longest one fetch's deferred work may run (#1804). Past it the work is dropped, which cancels
/// it at its next await, as the fetch's own deadline did when this work ran inline. On Android
/// the headless fetch holds the exclusive state lock and the open database until its tail ends,
/// so this also bounds how long an app launch can wait behind it. It stays under the host's
/// settle wait (`RichPushHandler.SETTLE_MS`, 20 s), so the tail always ends while the push
/// foreground service still holds the network open, with room left for its LiveKit hint.
/// (Unit tests shorten it so the overrun test runs in real time.)
pub(crate) const TAIL_BUDGET: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(15)
};

/// What one push fetch read up front, and whether its follow-up work may wait.
pub(crate) struct Scope {
    prefetch: Option<Prefetch>,
    /// Where deferred work goes; `None` runs it inline.
    tail: Option<Arc<TailSet>>,
}

/// The follow-up work one fetch deferred. Its owner decides what waits for it
/// (the headless fetch keeps the state lock and the database open until it ends).
#[derive(Default)]
pub(crate) struct TailSet(Mutex<Vec<tokio::task::JoinHandle<()>>>);

impl TailSet {
    pub(crate) fn take(&self) -> Vec<tokio::task::JoinHandle<()>> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

struct Prefetch {
    /// The conversation the handle named; the catch-up also answers for its MLS group id.
    conversation_id: String,
    catch_up: Option<CatchUpResponse>,
    /// Envelopes are handed out once.
    envelopes_taken: Mutex<bool>,
    welcomes: Option<FetchWelcomesResponse>,
    account_keys: Vec<AccountKeyRow>,
    account_keys_for: Vec<String>,
}

impl Scope {
    /// A scope with a prefetch, or `None` for the prefetch when it may not be used.
    pub(crate) fn new(resp: Option<PushFetchResponse>, tail: Option<Arc<TailSet>>) -> Arc<Self> {
        let prefetch = resp.and_then(|r| {
            let conversation_id = r.conversation_id?;
            // A pending Welcome is applied from a fresh read under the lock (see the module doc).
            if r.welcomes.as_ref().is_some_and(|w| !w.welcomes.is_empty()) {
                return None;
            }
            Some(Prefetch {
                conversation_id,
                catch_up: r.catch_up,
                envelopes_taken: Mutex::new(false),
                welcomes: r.welcomes,
                account_keys: r.account_keys,
                account_keys_for: r.account_keys_for,
            })
        });
        Arc::new(Self { prefetch, tail })
    }

    pub(crate) fn has_prefetch(&self) -> bool {
        self.prefetch.is_some()
    }
}

/// Run `fut` inside `scope`.
pub(crate) async fn run<F: Future>(scope: Arc<Scope>, fut: F) -> F::Output {
    SCOPE.scope(scope, fut).await
}

fn current() -> Option<Arc<Scope>> {
    SCOPE.try_with(Arc::clone).ok()
}

/// The prefetched catch-up for `conversation_id`, as `/v1/conversations/catch-up`
/// would answer these flags, or `None` to ask the network.
pub(crate) fn catch_up(
    conversation_id: &str,
    want_envelopes: bool,
    want_dm: bool,
) -> Option<CatchUpResponse> {
    let scope = current()?;
    let p = scope.prefetch.as_ref()?;
    let resp = p.catch_up.as_ref()?;
    serve_catch_up(p, resp, conversation_id, want_envelopes, want_dm)
}

fn serve_catch_up(
    p: &Prefetch,
    resp: &CatchUpResponse,
    conversation_id: &str,
    want_envelopes: bool,
    want_dm: bool,
) -> Option<CatchUpResponse> {
    if conversation_id != p.conversation_id && conversation_id != resp.mls_group_id {
        return None;
    }
    if want_dm && resp.kind == pollis_api::directory::ConversationKind::Dm && resp.dm.is_none() && resp.authorized {
        return None;
    }
    if want_envelopes {
        // `head` is present exactly when envelopes were read.
        if resp.head.is_none() && resp.authorized {
            return None;
        }
        let mut taken = p.envelopes_taken.lock().unwrap_or_else(|e| e.into_inner());
        if *taken {
            return None;
        }
        *taken = true;
        return Some(resp.clone());
    }
    let mut out = resp.clone();
    out.envelopes = Vec::new();
    out.head = None;
    Some(out)
}

/// The prefetched Welcome read (always empty: see [`Scope::new`]), or `None`.
pub(crate) fn welcomes() -> Option<FetchWelcomesResponse> {
    let scope = current()?;
    scope.prefetch.as_ref()?.welcomes.clone()
}

/// The prefetched account keys, when every id asked about was read.
pub(crate) fn account_keys(user_ids: &[String]) -> Option<Vec<AccountKeyRow>> {
    let scope = current()?;
    let p = scope.prefetch.as_ref()?;
    if !user_ids.iter().all(|u| p.account_keys_for.contains(u)) {
        return None;
    }
    Some(
        p.account_keys
            .iter()
            .filter(|k| user_ids.contains(&k.user_id))
            .cloned()
            .collect(),
    )
}

/// Follow-up work still running, oldest first: what [`settle`] waits for.
static TAIL: Mutex<Vec<tokio::task::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Run `work` now, or, inside a scope that defers, in the background into its [`TailSet`].
pub(crate) async fn defer_or_run<F>(work: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    match current().and_then(|s| s.tail.clone()) {
        Some(set) => {
            let handle = tokio::spawn(IN_TAIL.scope((), async move {
                if tokio::time::timeout(TAIL_BUDGET, work).await.is_err() {
                    crate::diag!("[notify] deferred posts ran past {TAIL_BUDGET:?}; dropped");
                }
            }));
            set.0.lock().unwrap_or_else(|e| e.into_inner()).push(handle);
        }
        None => work.await,
    }
}

/// Spawn fire-and-forget work (a LiveKit wake hint). Spawned from deferred work, it is kept for
/// [`settle`], so the push foreground service is not released while it is still in flight.
/// True when it was kept. Only the mobile LiveKit stub sends hints this way.
#[cfg_attr(feature = "media", allow(dead_code))]
pub(crate) fn spawn_in_tail<F>(work: F) -> bool
where
    F: Future<Output = ()> + Send + 'static,
{
    let in_tail = IN_TAIL.try_with(|_| ()).is_ok();
    let handle = tokio::spawn(work);
    if in_tail {
        track(handle);
    }
    in_tail
}

/// Wait for `pending`, then run `after` (e.g. close the database and release the lock), as
/// one task [`settle`] waits for.
pub(crate) fn finish_after<F>(pending: Vec<tokio::task::JoinHandle<()>>, after: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    track(tokio::spawn(async move {
        for handle in pending {
            let _ = handle.await;
        }
        after.await;
    }));
}

/// Keep `handle` until [`settle`] has seen it finish.
pub(crate) fn track(handle: tokio::task::JoinHandle<()>) {
    let mut tail = TAIL.lock().unwrap_or_else(|e| e.into_inner());
    tail.retain(|h| !h.is_finished());
    tail.push(handle);
}

/// Wait up to `timeout` for every deferred tail to finish. True when none is left.
pub async fn settle(timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let pending: Vec<_> = {
            let mut tail = TAIL.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *tail)
        };
        if pending.is_empty() {
            return true;
        }
        for handle in pending {
            if tokio::time::timeout_at(deadline, handle).await.is_err() {
                // Out of time: it keeps running; nothing waits on it any more.
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_api::directory::{ConversationKind, EnvelopeWire, LogHead};

    fn envelope(id: &str) -> EnvelopeWire {
        serde_json::from_value(serde_json::json!({
            "conversation_id": "dm-1", "id": id, "sender_id": "s", "ciphertext": "ct",
            "sent_at": "2026-01-01", "kind": "message", "seq": 1
        }))
        .unwrap()
    }

    fn response(dm: bool) -> PushFetchResponse {
        PushFetchResponse {
            conversation_id: Some("chan-1".into()),
            kind: Some(if dm { "dm" } else { "channel" }.into()),
            welcomes: Some(FetchWelcomesResponse {
                welcomes: Vec::new(),
                unclaimed_key_packages: Some(5),
                has_last_resort_key_package: Some(true),
            }),
            catch_up: Some(CatchUpResponse {
                authorized: true,
                mls_group_id: "group-1".into(),
                kind: ConversationKind::Channel,
                conversation_ids: vec!["chan-1".into()],
                envelopes: vec![envelope("e1")],
                dm: None,
                roster: Vec::new(),
                head: Some(LogHead { generation: 0, epoch: 3 }),
                membership_version: None,
            }),
            account_keys: vec![AccountKeyRow {
                user_id: "bob".into(),
                account_id_pub: "a2V5".into(),
                identity_version: 1,
            }],
            account_keys_for: vec!["bob".into(), "carol".into()],
        }
    }

    #[tokio::test]
    async fn envelopes_are_served_once_and_metadata_without_them() {
        let scope = Scope::new(Some(response(false)), None);
        run(scope, async {
            // The group lookup by channel id: metadata, no envelopes.
            let meta = catch_up("chan-1", false, false).expect("served");
            assert!(meta.envelopes.is_empty());
            assert!(meta.head.is_none());
            // The catch-up by MLS group id: the envelopes, once.
            let first = catch_up("group-1", true, false).expect("served");
            assert_eq!(first.envelopes.len(), 1);
            assert!(catch_up("group-1", true, false).is_none(), "a second pass reads the network");
            // Another conversation is never answered from this prefetch.
            assert!(catch_up("other", false, false).is_none());
            // Welcomes and keys.
            assert_eq!(welcomes().expect("served").unclaimed_key_packages, Some(5));
            assert_eq!(account_keys(&["bob".into()]).expect("served").len(), 1);
            // Carol was read and has no key: an empty answer, not a network read.
            assert_eq!(account_keys(&["carol".into()]).expect("served").len(), 0);
            assert!(account_keys(&["dave".into()]).is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn a_pending_welcome_disables_the_prefetch() {
        let mut r = response(false);
        r.welcomes.as_mut().unwrap().welcomes.push(pollis_api::reads::PendingWelcome {
            id: "w1".into(),
            welcome: "d2VsY29tZQ==".into(),
            conversation_id: None,
        });
        let scope = Scope::new(Some(r), None);
        assert!(!scope.has_prefetch());
        run(scope, async {
            assert!(catch_up("chan-1", true, false).is_none());
            assert!(welcomes().is_none());
            assert!(account_keys(&["bob".into()]).is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn nothing_is_served_outside_a_scope() {
        assert!(catch_up("chan-1", false, false).is_none());
        assert!(welcomes().is_none());
    }

    #[tokio::test]
    async fn a_dm_catch_up_without_members_is_not_served_for_want_dm() {
        let mut r = response(true);
        r.catch_up.as_mut().unwrap().kind = ConversationKind::Dm;
        let scope = Scope::new(Some(r), None);
        run(scope, async {
            assert!(catch_up("chan-1", false, true).is_none());
        })
        .await;
    }

    /// `settle` drains one process-wide list, so the tests that call it take turns.
    static SETTLE_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn a_deferring_scope_runs_the_tail_after_and_settle_waits_for_it() {
        let _turn = SETTLE_TESTS.lock().await;
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let set = Arc::new(TailSet::default());
        let scope = Scope::new(None, Some(Arc::clone(&set)));
        run(scope, async move {
            defer_or_run(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await;
        })
        .await;
        assert!(!done.load(std::sync::atomic::Ordering::SeqCst), "deferred, not awaited");
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = Arc::clone(&released);
        finish_after(set.take(), async move { r.store(true, std::sync::atomic::Ordering::SeqCst) });
        assert!(settle(Duration::from_secs(5)).await);
        assert!(released.load(std::sync::atomic::Ordering::SeqCst), "the closer ran after the tail");
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn outside_a_deferring_scope_the_tail_runs_inline() {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&done);
        defer_or_run(async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .await;
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[tokio::test]
    async fn a_deferred_tail_past_its_budget_is_dropped() {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let set = Arc::new(TailSet::default());
        run(Scope::new(None, Some(Arc::clone(&set))), async move {
            defer_or_run(async move {
                tokio::time::sleep(TAIL_BUDGET * 4).await;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .await;
        })
        .await;
        let started = tokio::time::Instant::now();
        for handle in set.take() {
            handle.await.expect("the tail task ends on its own");
        }
        assert!(started.elapsed() < TAIL_BUDGET * 3, "bounded by the budget");
        assert!(!done.load(std::sync::atomic::Ordering::SeqCst), "the overrunning work was dropped");
    }

    #[tokio::test]
    async fn a_hint_spawned_by_deferred_work_is_settled() {
        let _turn = SETTLE_TESTS.lock().await;
        let sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tracked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, t) = (Arc::clone(&sent), Arc::clone(&tracked));
        let set = Arc::new(TailSet::default());
        run(Scope::new(None, Some(Arc::clone(&set))), async move {
            defer_or_run(async move {
                let kept = spawn_in_tail(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    s.store(true, std::sync::atomic::Ordering::SeqCst);
                });
                t.store(kept, std::sync::atomic::Ordering::SeqCst);
            })
            .await;
        })
        .await;
        finish_after(set.take(), async {});
        assert!(settle(Duration::from_secs(5)).await);
        assert!(tracked.load(std::sync::atomic::Ordering::SeqCst), "kept for settle");
        assert!(sent.load(std::sync::atomic::Ordering::SeqCst), "settle waited for the hint");
    }

    #[tokio::test]
    async fn a_hint_outside_deferred_work_is_not_kept() {
        assert!(!spawn_in_tail(async {}));
    }

}
