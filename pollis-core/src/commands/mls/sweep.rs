//! Cold-launch (or post-reconnect) MLS catch-up sweep (#371), plus the
//! reconcile backstop that retries dropped evictions (#430) and re-adds
//! leafless devices (#1412, #1571). See `.codesight/wiki/mls.md`.

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

    let dirs = crate::commands::ds_reads::conversations(state, user_id).await?;
    let group_ids: Vec<String> = dirs.group_ids;
    let dm_ids: Vec<String> = dirs.dm_ids;

    crate::diag!(
        "[mls-sweep] {user_id}: {} group(s), {} dm(s)",
        group_ids.len(),
        dm_ids.len()
    );

    // Per-sweep budgets, so an overdue-everywhere device doesn't turn one cold
    // launch into a commit storm.
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

/// What [`reconcile_backstop_all`] did.
#[derive(Debug, Default, serde::Serialize)]
pub struct BackstopReport {
    /// Conversations looked at.
    pub checked: usize,
    /// Conversations whose tree had drifted and were reconciled.
    pub reconciled: usize,
}

/// The reconcile backstop for every conversation, without the sweep's catch-up,
/// migration and self-update (#1571).
///
/// Mobile's entry point: it runs right after the app's own ingest pass, which
/// has already caught each conversation up.
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

/// Before sealing, reconcile `conversation_id` if its tree has drifted (#1571).
/// At most once per conversation per [`PRE_SEAL_CHECK_INTERVAL`]; never fails a
/// send.
///
/// The caller must have just caught the conversation up and hold no MLS lock.
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

/// One conversation's sweep pass. Ordering is load-bearing:
///
/// 1. Interleaved catch-up, so messages sealed at epochs we advance past are
///    still decrypted.
/// 2. Reconcile backstop, before migration so the roster is settled before we
///    decide who comes across.
/// 3. Migration backstop (#454 P4).
/// 4. Self-update backstop (#666), last because a landed migration already
///    merged our leaf, making it a no-op.
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

/// Post-quantum suite migration for one conversation (#454 P4).
///
/// The only driver of migration: eligibility becomes true silently as the fleet
/// updates, so every sweep re-asks. `budget` is shared across the sweep.
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
        Ok(true) => {
            *budget += 1;
        }
        Ok(false) => {}
        Err(e) => {
            crate::diag!("[mls-sweep] suite migration for {conversation_id}: {e}");
        }
    }
}

/// Periodic own-leaf rotation for one conversation (#666): the source of
/// post-compromise security, and the catch-all for a deferred post-join rotation.
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
/// conversations, in the order `groups::membership` uses:
///
/// 1. Poll Welcomes: this device may be the one just added.
/// 2. Interleaved catch-up, so messages at epochs we pass are still decrypted.
/// 3. On a leave only, stage the remove: a leaver cannot commit its own removal
///    (#1081). Several members racing here converge through the DS CAS.
///
/// Best-effort throughout.
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

/// Reconcile `conversation_id` only if the local tree holds a leaf reconcile
/// would evict. The cheap precheck is what lets a wake-up path run it.
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

/// [`reconcile_if_tree_has_stale_leaf`] plus the add half (#1412). Answers
/// whether it reconciled.
///
/// A skipped device is derived on every pass, never recorded: a record could
/// disagree with the tree after a concurrent commit. Not run from the membership
/// wake, where every member would race and burn claims from a pool that just ran
/// dry.
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

/// What the cheap pre-check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeDrift {
    /// The tree is what reconcile would make it.
    Settled,
    /// A leaf reconcile would remove ([`has_stale_leaf`]).
    StaleLeaf,
    /// A device reconcile would add ([`has_addable_device`]).
    MissingDevice,
}

/// Would reconcile add a device right now (#1412)?
///
/// Derived from [`reconcile::desired_set`], so it can never predict a different
/// add set than the reconcile it gates. `claimable` must be scoped to the
/// group's suite, as reconcile's own claim is.
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

/// Does a live device of a roster member hold no leaf? False in steady state,
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
/// Derived from [`reconcile::desired_set`] (with no KeyPackages: only retention
/// matters) so the precheck can never drift from reconcile's rule. It adds one
/// test `desired_set` cannot make: two leaves claiming one `(user, device)`,
/// the shape of a rogue leaf grafted by an external join (#1161 M7/H4).
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

/// Cheap pre-check for the reconcile backstop: does the local tree differ from
/// what reconcile would make it? Adds are considered only with
/// `include_additions`.
///
/// The steady-state answer costs a local MLS load and two small reads; the
/// KeyPackage read happens only when a roster device lacks a leaf.
async fn local_tree_drift(
    state: &Arc<AppState>,
    conversation_id: &str,
    include_additions: bool,
) -> Result<TreeDrift> {
    // Scoped so the !Send group never crosses an await.
    let (tree_members, suite): (Vec<(String, String)>, _) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => return Ok(TreeDrift::Settled),
        };
        // Storage-only read, so no suite dispatch is needed.
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

    // The same roster `reconcile_group_mls_impl` diffs against, including
    // pending invitees so their leaves are never mistaken for stale ones.
    let roster: HashSet<String> =
        crate::commands::ds_reads::catch_up_full(state, conversation_id, false, false, true)
            .await?
            .roster
            .into_iter()
            .collect();

    let roster_ids: Vec<String> = roster.iter().cloned().collect();
    let valid_devices = crate::commands::ds_reads::registered_devices(state, &roster_ids).await?;

    if has_stale_leaf(&tree_members, &roster, &valid_devices) {
        return Ok(TreeDrift::StaleLeaf);
    }
    if !include_additions || !has_leafless_device(&tree_members, &roster, &valid_devices) {
        return Ok(TreeDrift::Settled);
    }

    // Same read and suite scoping as reconcile's own claim.
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

    /// Revoking one device must not drag its live sibling out with it (#1083).
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

    /// A duplicate leaf for a real, on-roster device is stale (#1161 M7).
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

    /// The device whose pool ran dry, once it has a KeyPackage again (#1412).
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

    /// `desired_set` adds neither a revoked device nor an off-roster user (#679).
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

    /// Spare packages on a device already in the tree are the normal state.
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
