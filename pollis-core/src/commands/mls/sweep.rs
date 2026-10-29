//! Cold-launch (or post-reconnect) MLS catch-up sweep.
//!
//! Catches every MLS group up (decrypting en route) before the user can take any MLS action (#371).
//!
//! Best-effort per group: one failure never blocks the rest.
//!
//! ## Eviction/remove reconcile backstop (issue #430 P1)
//!
//! A dropped remove leaves the removed device in every local tree, still sealed to: a forward-secrecy gap.
//!
//! So the sweep reconciles too, gated by the cheap [`local_tree_has_stale_leaf`] pre-check.
//!
//! ## Missing-device re-add backstop (issue #1412)
//!
//! It also re-adds a leafless device whose KeyPackage pool was empty at add time.
//!
//! ## On mobile, and before every seal (#1571)
//!
//! Mobile never runs the sweep, so two more callers reach the backstop:
//!
//! * [`reconcile_backstop_all`]: every conversation, after mobile's ingest pass;
//! * [`backstop_before_seal`]: one conversation, before a seal, at most once per [`PRE_SEAL_CHECK_INTERVAL`].

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

use super::provider::{load_stored_group, parse_credential_device_id, parse_credential_user_id};

pub async fn catch_up_all_mls_groups(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let device_id = state.device_id.lock().await.clone();
    if let Some(ref did) = device_id {
        if let Err(e) =
            crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
        {
            crate::diag!("[mls-sweep] poll_mls_welcomes: {e}");
        }
    }

    // Id-only enumeration in ONE request (#987).
    let dirs = crate::commands::ds_reads::conversations(state, user_id).await?;
    let group_ids: Vec<String> = dirs.group_ids;
    let dm_ids: Vec<String> = dirs.dm_ids;

    crate::diag!(
        "[mls-sweep] {user_id}: {} group(s), {} dm(s)",
        group_ids.len(),
        dm_ids.len()
    );

    // Capped so an overdue-everywhere device does not storm the log.
    let mut self_updates = 0usize;
    // Capped more sharply: migration is the sweep's most expensive step (#454 P4).
    let mut migrations = 0usize;

    // Regular groups: mls_group_id IS the group id.
    for gid in &group_ids {
        sweep_conversation(state, gid, user_id, "group", &mut self_updates, &mut migrations).await;
    }

    // DMs: mls_group_id IS the dm_channel_id — a single-conversation MLS group.
    for did in &dm_ids {
        sweep_conversation(state, did, user_id, "dm", &mut self_updates, &mut migrations).await;
    }

    Ok(())
}

/// What [`reconcile_backstop_all`] did.
#[derive(Debug, Default, serde::Serialize)]
pub struct BackstopReport {
    /// Conversations looked at.
    pub checked: usize,
    /// Conversations whose tree had drifted and were reconciled.
    pub reconciled: usize,
}

/// The reconcile backstop for every conversation the user is in, without the
/// catch-up, migration and self-update the full sweep adds (#1571).
///
/// Mobile's entry, run after its own ingest pass; cheap in steady state (see [`local_tree_drift`]).
pub async fn reconcile_backstop_all(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<BackstopReport> {
    let dirs = crate::commands::ds_reads::conversations(state, user_id).await?;
    let mut report = BackstopReport::default();
    for conversation_id in dirs.group_ids.iter().chain(dirs.dm_ids.iter()) {
        report.checked += 1;
        match reconcile_if_tree_drifted(state, conversation_id, user_id).await {
            Ok(true) => report.reconciled += 1,
            Ok(false) => {}
            Err(e) => {
                crate::diag!("[mls-backstop] {conversation_id}: {e}");
            }
        }
    }
    crate::diag!(
        "[mls-backstop] {user_id}: {} conversation(s) checked, {} reconciled",
        report.checked,
        report.reconciled
    );
    Ok(report)
}

/// The reconcile backstop for ONE conversation: reconcile it only if its local
/// tree has drifted from what reconcile wants. Answers whether it reconciled.
pub async fn reconcile_backstop_one(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> Result<bool> {
    reconcile_if_tree_drifted(state, conversation_id, user_id).await
}

/// How long a sender trusts its last pre-seal check of a conversation.
pub(crate) const PRE_SEAL_CHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);

/// Before sealing into `conversation_id`, re-add any device that belongs in its
/// tree and is missing — or evict a stale leaf — so the message is sealed to the
/// tree reconcile wants (#1571). At most once per conversation per
/// [`PRE_SEAL_CHECK_INTERVAL`] on this device; best-effort, never fails a send.
///
/// The caller has caught up and holds no MLS lock.
pub async fn backstop_before_seal(state: &Arc<AppState>, conversation_id: &str, user_id: &str) {
    {
        let mut checked = state.pre_seal_checked.lock().await;
        let now = std::time::Instant::now();
        if let Some(at) = checked.get(conversation_id) {
            if now.duration_since(*at) < PRE_SEAL_CHECK_INTERVAL {
                return;
            }
        }
        checked.insert(conversation_id.to_string(), now);
    }
    if let Err(e) = reconcile_if_tree_drifted(state, conversation_id, user_id).await {
        crate::diag!("[mls-backstop] pre-seal check for {conversation_id}: {e}");
    }
}

/// One conversation's full sweep pass. Ordering is load-bearing:
///
/// 1. **Catch up** via the interleaved path, so nothing sealed before a membership change is lost.
/// 2. **Reconcile backstop** (#430 P1, #1412), before migration so the roster is settled.
/// 3. **Migration backstop** — move to the hybrid suite if the whole roster can
///    now follow (#454 P4).
/// 4. **Self-update backstop** (#666), last: a migration already records the rotation.
///
/// Best-effort throughout.
async fn sweep_conversation(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    kind: &str,
    self_updates: &mut usize,
    migrations: &mut usize,
) {
    if let Err(e) =
        crate::commands::messages::catch_up_mls_group_interleaved(state, conversation_id, user_id)
            .await
    {
        crate::diag!("[mls-sweep] catch_up_mls_group for {kind} {conversation_id}: {e}");
    }
    if let Err(e) = reconcile_if_tree_drifted(state, conversation_id, user_id).await {
        crate::diag!("[mls-sweep] reconcile backstop for {kind} {conversation_id}: {e}");
    }
    migrate_backstop(state, conversation_id, user_id, migrations).await;
    self_update_backstop(state, conversation_id, user_id, self_updates).await;
}

/// Post-quantum suite migration for a single conversation (#454 P4).
///
/// Best-effort and capped by the shared `budget`.
async fn migrate_backstop(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    budget: &mut usize,
) {
    if *budget >= super::migrate::MAX_MIGRATIONS_PER_SWEEP {
        return;
    }
    match super::migrate::migrate_to_current_suite_if_due(state, conversation_id, user_id).await {
        // Only a landed migration spends budget: an ineligible conversation did
        // no work, and a lost race left it eligible for the next sweep to retry.
        Ok(true) => {
            *budget += 1;
        }
        Ok(false) => {}
        Err(e) => {
            crate::diag!("[mls-sweep] suite migration for {conversation_id}: {e}");
        }
    }
}

/// Periodic own-leaf rotation for a single conversation (issue #666).
///
/// Bounds how long a stolen leaf key keeps reading: post-compromise security needs this device's own commit.
///
/// Also catches post-join rotations the live path missed.
///
/// Best-effort and capped by the shared `budget`.
async fn self_update_backstop(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    budget: &mut usize,
) {
    if *budget >= super::self_update::MAX_SELF_UPDATES_PER_SWEEP {
        return;
    }
    match super::self_update::self_update_if_due(state, conversation_id, user_id).await {
        // Only a landed commit spends budget: a not-due group did no work, and a
        // lost race left the rotation due for the next sweep to retry.
        Ok(true) => {
            *budget += 1;
            crate::diag!("[mls-sweep] {conversation_id}: rotated own leaf");
        }
        Ok(false) => {}
        Err(e) => {
            crate::diag!("[mls-sweep] self-update for {conversation_id}: {e}");
        }
    }
}

/// Everything a device does when a `membership_changed` wake-up names one of its conversations.
///
/// Order matters, and it is the same order `groups::membership` uses:
///
/// 1. **Poll Welcomes** — this device may be the one that was just added.
/// 2. **Interleaved group catch-up**, so no un-ingested message's keys are dropped (`max_past_epochs = 0`).
/// 3. **On a leave only**, stage the remove: the leaver cannot remove itself (#1081); racing members converge via `LostRace`.
///
/// Every step is best-effort.
pub async fn apply_membership_wake(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    is_leave: bool,
) {
    let device_id = state.device_id.lock().await.clone();
    if let Some(ref device_id) = device_id {
        if let Err(e) =
            crate::commands::mls::poll_mls_welcomes_inner(state, user_id, device_id).await
        {
            crate::diag!("[mls] poll_welcomes for {conversation_id}: {e}");
        }
    }

    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
        state,
        conversation_id,
        user_id,
    )
    .await
    {
        crate::diag!("[mls] catch_up_mls_group for {conversation_id}: {e}");
    }

    if is_leave {
        if let Err(e) = reconcile_if_tree_has_stale_leaf(state, conversation_id, user_id).await {
            crate::diag!("[mls] leave reconcile for {conversation_id}: {e}");
        }
    }
}

/// Reconcile `conversation_id` only if the local tree actually holds a leaf a
/// reconcile would evict. Two callers, both of which want exactly this shape:
///
/// * the sweep's backstop, retrying a remove commit that was dropped;
/// * [`apply_membership_wake`] on a **leave**, where nobody has staged the remove yet (#1081).
///
/// The precheck keeps it cheap enough for a wake-up path.
async fn reconcile_if_tree_has_stale_leaf(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> Result<()> {
    if local_tree_drift(state, conversation_id, false).await? == TreeDrift::Settled {
        return Ok(());
    }

    crate::diag!(
        "[mls-sweep] {conversation_id}: stale leaf in local tree — staging the remove/eviction reconcile"
    );
    crate::commands::mls::reconcile_group_mls_impl(state, conversation_id, user_id).await?;
    Ok(())
}

/// The sweep's reconcile backstop: everything [`reconcile_if_tree_has_stale_leaf`]
/// retries, plus the ADD half (#1412).
///
/// A device skipped for an empty pool is DERIVED each pass (live, leafless, claimable), never recorded, so it cannot disagree with the tree.
///
/// Several members may race to add it; the DS CAS picks one.
///
/// Not run from the membership wake, where every member would race and burn claims.
///
/// Answers whether it reconciled.
pub(crate) async fn reconcile_if_tree_drifted(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> Result<bool> {
    let drift = local_tree_drift(state, conversation_id, true).await?;
    if drift == TreeDrift::Settled {
        return Ok(false);
    }

    crate::diag!(
        "[mls-sweep] {conversation_id}: {drift:?} — staging a reconcile"
    );
    crate::commands::mls::reconcile_group_mls_impl(state, conversation_id, user_id).await?;
    Ok(true)
}

/// What the cheap pre-check found, in the order it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeDrift {
    /// The tree is what reconcile would make it.
    Settled,
    /// A leaf reconcile would remove ([`has_stale_leaf`]).
    StaleLeaf,
    /// A device reconcile would add ([`has_addable_device`]).
    MissingDevice,
}

/// Would reconcile ADD anyone — is a device reconcile wants in the tree, and
/// could add right now, missing from it (#1412)?
///
/// Derived from [`reconcile::desired_set`], so it predicts exactly reconcile's add set; `claimable` (incl. last-resort packages, #1571) is the KeyPackage half.
fn has_addable_device(
    tree_members: &[(String, String)],
    roster: &HashSet<String>,
    valid_devices: &HashSet<(String, String)>,
    claimable: &[(String, String)],
) -> bool {
    let desired = super::reconcile::desired_set(
        claimable,
        tree_members.iter(),
        roster,
        Some(valid_devices),
    );
    desired.iter().any(|key| !tree_members.contains(key))
}

/// Live devices of roster members that hold no leaf — the candidates
/// [`has_addable_device`] then narrows to the ones with a KeyPackage. Empty in
/// steady state, which is what lets the pre-check skip the KeyPackage read.
fn has_leafless_device(
    tree_members: &[(String, String)],
    roster: &HashSet<String>,
    valid_devices: &HashSet<(String, String)>,
) -> bool {
    valid_devices
        .iter()
        .any(|key| roster.contains(&key.0) && !tree_members.contains(key))
}

/// Does the local tree hold a leaf that reconcile would remove?
///
/// Derived FROM [`reconcile::desired_set`], so the precheck cannot drift from reconcile's rule.
///
/// `kp_keys` is empty: only the retention half matters.
///
/// It adds a DUPLICATE test (#1161 M7/H4): two leaves claiming one real device, which a set-based test misses.
fn has_stale_leaf(
    tree_members: &[(String, String)],
    roster: &HashSet<String>,
    valid_devices: &HashSet<(String, String)>,
) -> bool {
    let desired = super::reconcile::desired_set(
        &[],
        tree_members.iter(),
        roster,
        Some(valid_devices),
    );
    if tree_members.iter().any(|key| !desired.contains(key)) {
        return true;
    }
    let distinct: HashSet<&(String, String)> = tree_members.iter().collect();
    distinct.len() != tree_members.len()
}

/// Cheap pre-check for the reconcile backstop: does the LOCAL MLS ratchet tree
/// differ from what a declarative reconcile would make it?
///
/// * a leaf reconcile would evict — a device whose user has left the roster, or
///   whose `user_device` row is gone ([`has_stale_leaf`]);
/// * with `include_additions`, a device reconcile would ADD — live, on the
///   roster, leafless, and holding a claimable KeyPackage in the group's suite
///   ([`has_addable_device`], #1412).
///
/// Settled in steady state using only a local load and two light reads; the KeyPackage read only when a device lacks a leaf.
async fn local_tree_drift(
    state: &Arc<AppState>,
    conversation_id: &str,
    include_additions: bool,
) -> Result<TreeDrift> {
    // 1. Local tree membership and the group's suite — a local SQLite read, no
    //    network. Scope the !Send provider/group so neither crosses an await.
    let (tree_members, suite): (Vec<(String, String)>, _) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            // No local group open (never joined / already forgotten): this
            // device has nothing to evict and cannot commit an add.
            None => return Ok(TreeDrift::Settled),
        };
        // Storage-only read; no crypto backend.
        match load_stored_group(db.conn(), conversation_id) {
            Some(group) => (
                group
                    .members()
                    .map(|m| {
                        let uid = parse_credential_user_id(&m.credential);
                        let did = parse_credential_device_id(&m.credential).unwrap_or_default();
                        (uid, did)
                    })
                    .collect(),
                group.ciphersuite(),
            ),
            // Missing / unreadable local group: nothing this device can commit.
            None => return Ok(TreeDrift::Settled),
        }
    };
    if tree_members.is_empty() {
        return Ok(TreeDrift::Settled);
    }

    // 2. Desired roster from the same server-side definition reconcile uses (#987), incl. pending invitees.
    let roster: HashSet<String> =
        crate::commands::ds_reads::catch_up_full(state, conversation_id, false, false, true)
            .await?
            .roster
            .into_iter()
            .collect();

    // 3. Registered devices for the roster, as reconcile uses.
    let roster_ids: Vec<String> = roster.iter().cloned().collect();
    let valid_devices = crate::commands::ds_reads::registered_devices(state, &roster_ids).await?;

    // 4. A leaf is stale iff reconcile would remove it — see [`has_stale_leaf`].
    if has_stale_leaf(&tree_members, &roster, &valid_devices) {
        return Ok(TreeDrift::StaleLeaf);
    }
    if !include_additions || !has_leafless_device(&tree_members, &roster, &valid_devices) {
        return Ok(TreeDrift::Settled);
    }

    // 5. A leafless device is worth a reconcile only with a claimable KeyPackage in this suite.
    let claimable = crate::commands::ds_reads::claimable_devices(
        state,
        &roster_ids,
        Some(i64::from(u16::from(suite))),
    )
    .await?;
    if has_addable_device(&tree_members, &roster, &valid_devices, &claimable) {
        return Ok(TreeDrift::MissingDevice);
    }
    Ok(TreeDrift::Settled)
}

#[cfg(test)]
mod tests {
    use super::{has_addable_device, has_leafless_device, has_stale_leaf};
    use std::collections::HashSet;

    fn key(u: &str, d: &str) -> (String, String) {
        (u.to_string(), d.to_string())
    }
    fn roster(users: &[&str]) -> HashSet<String> {
        users.iter().map(|u| u.to_string()).collect()
    }
    fn valid(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
        pairs.iter().map(|(u, d)| key(u, d)).collect()
    }

    #[test]
    fn a_settled_tree_has_nothing_stale() {
        assert!(!has_stale_leaf(
            &[key("alice", "a1"), key("bob", "b1")],
            &roster(&["alice", "bob"]),
            &valid(&[("alice", "a1"), ("bob", "b1")]),
        ));
    }

    /// The dropped-remove case the sweep exists to retry.
    #[test]
    fn a_leaf_whose_user_left_the_roster_is_stale() {
        assert!(has_stale_leaf(
            &[key("alice", "a1"), key("bob", "b1")],
            &roster(&["alice"]),
            &valid(&[("alice", "a1")]),
        ));
    }

    /// #1083: revoking one device must not drag its live sibling in.
    #[test]
    fn a_revoked_device_is_stale_but_its_live_sibling_is_not() {
        let tree = [key("alice", "a1"), key("alice", "a2")];
        let roster = roster(&["alice"]);

        assert!(
            has_stale_leaf(&tree, &roster, &valid(&[("alice", "a1")])),
            "the revoked a2 must register as stale"
        );
        // And with both live, nothing is stale — the sibling alone is not a reason.
        assert!(!has_stale_leaf(
            &tree,
            &roster,
            &valid(&[("alice", "a1"), ("alice", "a2")])
        ));
    }

    /// #1161 M7: two leaves claiming ONE real device is stale.
    #[test]
    fn two_leaves_claiming_one_device_are_stale() {
        assert!(has_stale_leaf(
            &[key("alice", "a1"), key("bob", "b1"), key("bob", "b1")],
            &roster(&["alice", "bob"]),
            &valid(&[("alice", "a1"), ("bob", "b1")]),
        ));
    }

    /// The committer's own leaf holds no KeyPackage and is not stale.
    #[test]
    fn a_retained_leaf_without_a_key_package_is_not_stale() {
        assert!(!has_stale_leaf(
            &[key("alice", "a1")],
            &roster(&["alice"]),
            &valid(&[("alice", "a1")]),
        ));
    }

    /// #1412: the device whose pool ran dry. It is live and on the roster but
    /// holds no leaf; once it has a KeyPackage again, reconcile would add it.
    #[test]
    fn a_leafless_device_with_a_key_package_is_addable() {
        let tree = [key("alice", "a1"), key("bob", "b1")];
        let roster = roster(&["alice", "bob"]);
        let valid = valid(&[("alice", "a1"), ("alice", "a2"), ("bob", "b1")]);
        assert!(has_leafless_device(&tree, &roster, &valid));
        assert!(has_addable_device(&tree, &roster, &valid, &[key("alice", "a2")]));
    }

    /// Still dry: reconcile could only skip it again.
    #[test]
    fn a_leafless_device_without_a_key_package_is_not_addable() {
        let tree = [key("alice", "a1"), key("bob", "b1")];
        let roster = roster(&["alice", "bob"]);
        let valid = valid(&[("alice", "a1"), ("alice", "a2"), ("bob", "b1")]);
        assert!(!has_addable_device(&tree, &roster, &valid, &[]));
    }

    /// A package from a revoked device, or from a user who is not on the roster,
    /// is not a reason to reconcile — `desired_set` would not add either (#679).
    #[test]
    fn revoked_or_off_roster_packages_are_not_addable() {
        let tree = [key("alice", "a1")];
        let roster = roster(&["alice"]);
        let valid = valid(&[("alice", "a1")]);
        assert!(!has_addable_device(
            &tree,
            &roster,
            &valid,
            &[key("alice", "a2"), key("mallory", "m1")],
        ));
        assert!(!has_leafless_device(&tree, &roster, &valid));
    }

    /// A device already in the tree keeps unclaimed packages; that is the
    /// normal state of every member and must read as settled.
    #[test]
    fn a_member_with_spare_key_packages_is_not_addable() {
        let tree = [key("alice", "a1"), key("bob", "b1")];
        let roster = roster(&["alice", "bob"]);
        let valid = valid(&[("alice", "a1"), ("bob", "b1")]);
        assert!(!has_leafless_device(&tree, &roster, &valid));
        assert!(!has_addable_device(
            &tree,
            &roster,
            &valid,
            &[key("alice", "a1"), key("bob", "b1")],
        ));
    }
}
