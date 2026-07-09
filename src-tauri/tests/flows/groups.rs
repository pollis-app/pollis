use crate::harness::{wipe, world, TestClient};
use serde_json::json;
use serial_test::serial;

// ─── Smoke test ─────────────────────────────────────────────────────────────

/// Minimal end-to-end path: one client signs up, creates a group, and can
/// list it back. Validates the whole stack — Config → AppState → RemoteDb →
/// keystore → MLS init → Turso round-trip — on real code paths.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn single_client_signup_and_create_group() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    assert_eq!(profile.email, "alice@test.local");
    assert!(!profile.id.is_empty());
    assert!(!profile.enrollment_required, "new user should not need enrollment");

    let group: serde_json::Value = alice
        .invoke_json(
            "create_group",
            json!({ "name": "Test Group", "description": null, "ownerId": alice.user_id() }),
        )
        .await;
    assert_eq!(group["name"], "Test Group");
    assert_eq!(group["owner_id"], alice.user_id());

    let groups: serde_json::Value = alice
        .invoke_json("list_user_groups", json!({ "userId": alice.user_id() }))
        .await;
    let groups = groups.as_array().expect("groups array");
    assert_eq!(groups.len(), 1, "should see the group we just created");
    assert_eq!(groups[0]["name"], "Test Group");

    // Force the borrow checker to keep the client alive past the final
    // assertion (otherwise it may be dropped early in some builds, closing
    // the local DB handle mid-assertion).
    drop(alice);
}

/// Two distinct clients in the same test process. Proves:
///   - Per-client `InMemoryKeystore`s keep sessions isolated.
///   - Both clients round-trip through the shared test Turso.
///   - `search_user_by_username` from one client resolves a user registered
///     by another — the basic read-after-write contract the invite / DM
///     flows are built on.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn two_clients_see_each_other_via_turso() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Distinct user IDs — keystores are independent.
    assert_ne!(alice_profile.id, bob_profile.id);

    // Bob can find Alice via the real search command.
    let hit: serde_json::Value = bob
        .invoke_json(
            "search_user_by_username",
            json!({ "username": alice_profile.username.clone() }),
        )
        .await;
    assert_eq!(hit["id"], alice_profile.id);
    assert_eq!(hit["username"], alice_profile.username);

    // And vice versa — proves the shared remote is symmetric, not just
    // single-writer.
    let hit: serde_json::Value = alice
        .invoke_json(
            "search_user_by_username",
            json!({ "username": bob_profile.username.clone() }),
        )
        .await;
    assert_eq!(hit["id"], bob_profile.id);

    drop(alice);
    drop(bob);
}

// ─── Scenarios ──────────────────────────────────────────────────────────────

/// Alice invites Bob by username. Bob sees the pending invite, accepts it,
/// and appears in the group member list.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_invite_accept_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Invite Test").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite = bob
        .first_pending_invite()
        .await
        .expect("bob should see one pending invite");
    assert_eq!(invite["group_id"], group_id);
    assert_eq!(invite["inviter_id"], alice_profile.id);

    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    // Bob should no longer see a pending invite.
    assert!(bob.first_pending_invite().await.is_none());

    // Both appear in the member list.
    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&alice_profile.id));
    assert!(ids.contains(&bob_profile.id));
    assert_eq!(ids.len(), 2);

    // Bob's own group list now includes the group.
    assert!(bob.list_group_ids().await.contains(&group_id));

    drop(alice);
    drop(bob);
}

/// Bob declines Alice's invite. The invite row goes away and Bob does not
/// become a member.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_invite_decline_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Decline Test").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite = bob.first_pending_invite().await.expect("one pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();

    bob.decline_invite(&invite_id).await;

    assert!(bob.first_pending_invite().await.is_none());

    // Membership unchanged — only Alice.
    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id]);
    assert!(!bob.list_group_ids().await.contains(&group_id));
    let _ = bob_profile;

    drop(alice);
    drop(bob);
}

/// Carol finds Alice's group by slug, requests access, and Alice (admin)
/// approves. Carol's request becomes non-pending and Carol gains membership.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_join_request_approve_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Joinable").await;

    carol.request_group_access(&group_id).await;

    // Admin sees one pending request for this group.
    let requests = alice.list_join_requests(&group_id).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["requester_id"], carol_profile.id);
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

    // Non-admins get an empty list (role gate in get_group_join_requests).
    assert!(carol.list_join_requests(&group_id).await.is_empty());

    alice.approve_join_request(&request_id).await;
    carol.poll().await;

    // Request no longer pending for the admin.
    assert!(alice.list_join_requests(&group_id).await.is_empty());

    // Carol is a member; Alice still is.
    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&alice_profile.id));
    assert!(ids.contains(&carol_profile.id));
    assert_eq!(ids.len(), 2);

    drop(alice);
    drop(carol);
}

/// Carol's request is rejected. She does not become a member and the pending
/// list clears.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_join_request_reject_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Not For You").await;

    carol.request_group_access(&group_id).await;
    let requests = alice.list_join_requests(&group_id).await;
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

    alice.reject_join_request(&request_id).await;

    assert!(alice.list_join_requests(&group_id).await.is_empty());
    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id]);
    assert!(!carol.list_group_ids().await.contains(&group_id));
    let _ = carol_profile;

    drop(alice);
    drop(carol);
}

/// After Alice removes Bob from the group, Bob no longer appears in the
/// member list and the group drops off his own list.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn removed_member_loses_access() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Kick Test").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite = bob.first_pending_invite().await.expect("pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    assert!(bob.list_group_ids().await.contains(&group_id));

    alice.remove_member(&group_id, &bob_profile.id).await;

    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id.clone()]);
    assert!(!bob.list_group_ids().await.contains(&group_id));

    drop(alice);
    drop(bob);
}

// ─── #261 Phase 2: directory-index equivalence ──────────────────────────────

/// After a representative membership-churn sequence, the directory index
/// (`user_groups` / `user_dms`) is an EXACT projection of the authoritative
/// `group_member` / `dm_channel_member` tables: every membership row is faithfully
/// projected (matching role, group name, accepted-state) and there are no orphan
/// index rows. This is the dual-write equivalence guard (#261 spec, acceptance D.1)
/// — it fails loudly if any DS write path forgets to maintain the index.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn directory_index_matches_membership() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Group churn: create, invite + accept (add member), promote to admin.
    let group_id = alice.create_group("Directory Index").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    alice
        .set_member_role(&group_id, &bob_profile.id, "admin")
        .await;

    // DM churn: create a request, accept it.
    let dm_id = alice.create_dm(&[bob_profile.id.as_str()]).await;
    bob.accept_dm_request(&dm_id).await;

    // Read raw state through the writable remote handle (sees every DS write).
    let conn = world().await.remote.conn().await.expect("remote conn");
    let count = |sql: &'static str| {
        let conn = &conn;
        async move {
            let mut rows = conn.query(sql, ()).await.expect("query");
            rows.next()
                .await
                .expect("rows")
                .expect("count row")
                .get::<i64>(0)
                .expect("i64")
        }
    };

    // Sanity: the churn actually produced index rows (no vacuous pass).
    assert!(count("SELECT COUNT(*) FROM user_groups").await >= 2, "user_groups populated");
    assert!(count("SELECT COUNT(*) FROM user_dms").await >= 2, "user_dms populated");

    // Every group_member row is faithfully projected (role + group name).
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM group_member gm JOIN groups g ON g.id = gm.group_id \
             WHERE NOT EXISTS ( \
                 SELECT 1 FROM user_groups ug \
                 WHERE ug.user_id = gm.user_id AND ug.group_id = gm.group_id \
                   AND ug.role = gm.role AND ug.group_name = g.name)"
        )
        .await,
        0,
        "every group_member row must be faithfully projected into user_groups"
    );
    // No orphan user_groups rows.
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM user_groups ug WHERE NOT EXISTS ( \
                 SELECT 1 FROM group_member gm \
                 WHERE gm.user_id = ug.user_id AND gm.group_id = ug.group_id)"
        )
        .await,
        0,
        "no orphan user_groups rows"
    );
    // Every dm_channel_member row is faithfully projected (NULL-safe accepted_at).
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM dm_channel_member dcm WHERE NOT EXISTS ( \
                 SELECT 1 FROM user_dms ud \
                 WHERE ud.user_id = dcm.user_id AND ud.dm_channel_id = dcm.dm_channel_id \
                   AND ud.accepted_at IS dcm.accepted_at)"
        )
        .await,
        0,
        "every dm_channel_member row must be faithfully projected into user_dms"
    );
    // No orphan user_dms rows.
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM user_dms ud WHERE NOT EXISTS ( \
                 SELECT 1 FROM dm_channel_member dcm \
                 WHERE dcm.user_id = ud.user_id AND dcm.dm_channel_id = ud.dm_channel_id)"
        )
        .await,
        0,
        "no orphan user_dms rows"
    );

    let _ = alice_profile;
    drop(alice);
    drop(bob);
}
