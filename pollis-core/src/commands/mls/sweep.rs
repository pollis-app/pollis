//! Cold-launch (or post-reconnect) MLS catch-up sweep.
//!
//! Catches every MLS group the user is in up to the server's epoch through the
//! interleaved (decrypting) path before the user can take any MLS action
//! (#371). Best-effort per group: one failure logs and the sweep continues.
//!
//! After each catch-up it runs a reconcile backstop gated by a cheap local
//! pre-check: a dropped remove leaves a removed leaf in the local tree, still
//! sealed for (#430 P1), and a device whose KeyPackage pool was empty when it
//! should have been added holds no leaf (#1412).

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

    // Own-leaf rotations and suite migrations issued this sweep, each capped so
    // one cold launch cannot become a commit storm.
    let mut self_updates = 0usize;
    let mut migrations = 0usize;

    for gid in &group_ids {
        sweep_conversation(state, gid, user_id, "group", &mut self_updates, &mut migrations).await;
    }

    for did in &dm_ids {
        sweep_conversation(state, did, user_id, "dm", &mut self_updates, &mut migrations).await;
    }

    Ok(())
}

/// One conversation's full sweep pass. Ordering is load-bearing:
///
/// 1. **Catch up** through the *interleaved* path, so a returning member
///    decrypts every message sealed at an epoch it is about to advance past.
/// 2. **Reconcile backstop** (#430 P1, #1412), before migration so the roster
///    is settled before we decide who has to come across.
/// 3. **Migration backstop** (#454 P4).
/// 4. **Self-update backstop** (#666), last: a migration records the rotation,
///    making this a no-op instead of a wasted commit on a retired lineage.
///
/// Best-effort throughout: every step logs and continues so one bad conversation
/// never blocks the rest of the sweep.
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

/// Suite migration for a single conversation (#454 P4). The sweep is its only
/// driver; `budget` is shared across the sweep so a device eligible everywhere
/// migrates a couple per launch.
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
        // Only a landed migration spends budget.
        Ok(true) => {
            *budget += 1;
        }
        Ok(false) => {}
        Err(e) => {
            crate::diag!("[mls-sweep] suite migration for {conversation_id}: {e}");
        }
    }
}

/// Periodic own-leaf rotation for a single conversation (#666): the source of
/// post-compromise security, and the catch-all for a missed post-join rotation.
/// `budget` is shared across the sweep.
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
        // Only a landed commit spends budget.
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

/// Everything a device does when a `membership_changed` wake-up names one of its
/// conversations, in the same order `groups::membership` uses:
///
/// 1. **Poll Welcomes**: this device may be the one that was just added.
/// 2. **Interleaved group catch-up** (`max_past_epochs = 0`: a bare replay would
///    drop un-ingested messages' keys).
/// 3. **On a leave only**, stage the remove: MLS forbids removing your own leaf,
///    so no one else has committed it yet (#1081). Several members racing here
///    resolve on the DS commit-log CAS.
///
/// Every step is best-effort and left for the next catch-up on failure.
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
/// reconcile would evict: the sweep's retry of a dropped remove, and
/// [`apply_membership_wake`] on a leave (#1081). The precheck keeps it cheap
/// enough for a wake-up path.
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
/// The missing device is DERIVED every pass (live, on the roster, leafless, with
/// a claimable KeyPackage), never recorded, so it cannot disagree with the tree.
/// Not run from the membership wake, where every member would race and burn
/// claims from the pool that just ran dry.
///
/// The caller has already run the interleaved catch-up, and no MLS lock is held:
/// reconcile takes its own.
async fn reconcile_if_tree_drifted(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> Result<()> {
    let drift = local_tree_drift(state, conversation_id, true).await?;
    if drift == TreeDrift::Settled {
        return Ok(());
    }

    crate::diag!(
        "[mls-sweep] {conversation_id}: {drift:?} — staging a reconcile"
    );
    crate::commands::mls::reconcile_group_mls_impl(state, conversation_id, user_id).await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeDrift {
    /// The tree is what reconcile would make it.
    Settled,
    /// A leaf reconcile would remove ([`has_stale_leaf`]).
    StaleLeaf,
    /// A device reconcile would add ([`has_addable_device`]).
    MissingDevice,
}

/// Would reconcile ADD a device right now (#1412)? Derived from
/// [`reconcile::desired_set`], with `claimable` (unclaimed packages in the
/// GROUP's suite) as its KeyPackage half, so the pre-check predicts exactly the
/// reconcile it gates.
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

/// Live devices of roster members that hold no leaf. Empty in steady state,
/// which lets the pre-check skip the KeyPackage read.
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
/// Derived FROM [`reconcile::desired_set`] (with no KeyPackages: only the
/// retention half) so the pre-check cannot drift from the reconcile it gates.
/// It adds the DUPLICATE test (#1161 M7/H4): a rogue leaf grafted by an
/// external join claims a real on-roster device, which only the per-leaf list
/// can see.
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

/// Cheap pre-check for the reconcile backstop: does the LOCAL ratchet tree
/// differ from what reconcile would make it ([`has_stale_leaf`], and with
/// `include_additions` [`has_addable_device`])?
///
/// Steady state costs a local MLS load and two roster/device reads; the
/// KeyPackage read happens only when some roster device lacks a leaf.
async fn local_tree_drift(
    state: &Arc<AppState>,
    conversation_id: &str,
    include_additions: bool,
) -> Result<TreeDrift> {
    // 1. Local tree membership and suite. Scope the !Send group so it does not
    //    cross an await.
    let (tree_members, suite): (Vec<(String, String)>, _) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => return Ok(TreeDrift::Settled),
        };
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
            None => return Ok(TreeDrift::Settled),
        }
    };
    if tree_members.is_empty() {
        return Ok(TreeDrift::Settled);
    }

    // 2. Desired roster, from the same server-side definition reconcile uses
    //    (#987). Pending invitees count, so their unjoined leaves are not stale.
    let roster: HashSet<String> =
        crate::commands::ds_reads::catch_up_full(state, conversation_id, false, false, true)
            .await?
            .roster
            .into_iter()
            .collect();

    // 3. Valid (user_id, device_id) pairs still registered for the roster.
    let roster_ids: Vec<String> = roster.iter().cloned().collect();
    let valid_devices = crate::commands::ds_reads::registered_devices(state, &roster_ids).await?;

    if has_stale_leaf(&tree_members, &roster, &valid_devices) {
        return Ok(TreeDrift::StaleLeaf);
    }
    if !include_additions || !has_leafless_device(&tree_members, &roster, &valid_devices) {
        return Ok(TreeDrift::Settled);
    }

    // 5. A leafless member is worth a reconcile only with a KeyPackage in THIS
    //    group's suite to claim.
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

    #[test]
    fn a_leaf_whose_user_left_the_roster_is_stale() {
        assert!(has_stale_leaf(
            &[key("alice", "a1"), key("bob", "b1")],
            &roster(&["alice"]),
            &valid(&[("alice", "a1")]),
        ));
    }

    /// #1083: revoking one device must not drag its live sibling with it.
    #[test]
    fn a_revoked_device_is_stale_but_its_live_sibling_is_not() {
        let tree = [key("alice", "a1"), key("alice", "a2")];
        let roster = roster(&["alice"]);

        assert!(
            has_stale_leaf(&tree, &roster, &valid(&[("alice", "a1")])),
            "the revoked a2 must register as stale"
        );
        assert!(!has_stale_leaf(
            &tree,
            &roster,
            &valid(&[("alice", "a1"), ("alice", "a2")])
        ));
    }

    /// #1161 M7: two leaves claiming ONE real `(user, device)` are stale.
    #[test]
    fn two_leaves_claiming_one_device_are_stale() {
        assert!(has_stale_leaf(
            &[key("alice", "a1"), key("bob", "b1"), key("bob", "b1")],
            &roster(&["alice", "bob"]),
            &valid(&[("alice", "a1"), ("bob", "b1")]),
        ));
    }

    /// The committer's own leaf holds no KeyPackage and must not read as stale.
    #[test]
    fn a_retained_leaf_without_a_key_package_is_not_stale() {
        assert!(!has_stale_leaf(
            &[key("alice", "a1")],
            &roster(&["alice"]),
            &valid(&[("alice", "a1")]),
        ));
    }

    /// #1412: the device whose pool ran dry is addable once it has a package.
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

    /// Neither a revoked device's nor an off-roster user's package is addable (#679).
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
