use crate::harness::{wipe, TestClient};
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

    // #875: a non-member used to receive `[]` here — the same answer as "nobody
    // has asked to join". It now reports the real condition.
    let err = carol
        .invoke_try(
            "get_group_join_requests",
            json!({ "groupId": group_id, "requesterId": carol.user_id() }),
        )
        .await
        .expect_err("a non-member must not receive a join-request list");
    assert!(
        err.contains("not a member of this group"),
        "non-member should be told why, got: {err}"
    );

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

/// #875 — the two admin-gated group READS used to answer `Ok(vec![])` for a
/// non-member and for a plain member alike, which is byte-identical to "there
/// is nothing here". A caller could not tell a permission boundary from an
/// empty page, and neither could the UI.
///
/// Both cases are exercised separately on purpose: "not a member at all" and
/// "a member without the role" are different conditions and must say so.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn join_requests_and_invite_links_are_admin_only() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Admin Only").await;

    // Bob joins as a plain member; Carol stays outside the group entirely.
    alice.invite(&group_id, &bob_profile.username).await;
    bob.poll().await;
    let invite = bob.first_pending_invite().await.expect("bob was invited");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    assert!(
        bob.list_group_ids().await.contains(&group_id),
        "bob should be a member before the role gate is tested"
    );

    // The admin can create and see an invite link.
    let created: serde_json::Value = alice
        .invoke_json(
            "create_group_invite_link",
            json!({ "groupId": group_id, "creatorId": alice.user_id(),
                    "expiresInHours": null, "maxUses": null }),
        )
        .await;
    assert!(created["token"].as_str().is_some(), "admin should get a link token");
    let links = alice
        .invoke_json(
            "list_group_invite_links",
            json!({ "groupId": group_id, "userId": alice.user_id() }),
        )
        .await;
    assert_eq!(
        links.as_array().expect("links array").len(),
        1,
        "the admin must still see the link — the fix must not deny the allowed case"
    );

    // A member without the role is told which role is missing …
    let bob_links = bob
        .invoke_try(
            "list_group_invite_links",
            json!({ "groupId": group_id, "userId": bob.user_id() }),
        )
        .await
        .expect_err("a non-admin member must not receive an invite-link list");
    assert!(
        bob_links.contains("only group admins can view invite links"),
        "a member should be told which role is missing, got: {bob_links}"
    );
    let bob_reqs = bob
        .invoke_try(
            "get_group_join_requests",
            json!({ "groupId": group_id, "requesterId": bob.user_id() }),
        )
        .await
        .expect_err("a non-admin member must not receive a join-request list");
    assert!(
        bob_reqs.contains("only group admins can view join requests"),
        "a member should be told which role is missing, got: {bob_reqs}"
    );

    // … and a non-member is told they are not in the group at all, which is a
    // different condition and used to be the same empty vec.
    let carol_links = carol
        .invoke_try(
            "list_group_invite_links",
            json!({ "groupId": group_id, "userId": carol.user_id() }),
        )
        .await
        .expect_err("a non-member must not receive an invite-link list");
    assert!(
        carol_links.contains("not a member of this group"),
        "a non-member should be told they are outside the group, got: {carol_links}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── #917 — group reads take a caller ────────────────────────────────────────

/// The roster, the channel list and the emoji list used to take a group id and
/// NO caller, so any signed-in user who named a group id got all three. Group
/// ids are enumerable (every client holds a whole-DB read-only Turso token, and
/// `search_group_by_slug` maps guessable names onto ids), so this was reachable
/// rather than theoretical.
///
/// All three are exercised in one scenario on purpose: they share a chokepoint
/// (`authz::require_member`), and a fix that guards two of three is the bug
/// again. Both sides of each are asserted — a guard that denied everyone would
/// pass a deny-only test while breaking the product.
///
/// Mallory is signed up and authenticated; she is simply outside the group. So
/// a denial here is "not a member", never "unknown user".
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_reads_are_members_only() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    mallory.sign_up("mallory@test.local").await;

    let group_id = alice.create_group("Private Group").await;

    // Bob joins as a plain member — the roster guard is about membership, not
    // about the admin role, so the allowed case must include a non-admin.
    alice.invite(&group_id, &bob_profile.username).await;
    bob.poll().await;
    let invite = bob.first_pending_invite().await.expect("bob was invited");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;

    crate::harness::seed_group_emoji(
        &group_id,
        "party_parrot",
        &"a".repeat(64),
        &alice_profile.id,
    )
    .await;

    // ── The allowed cases still work ────────────────────────────────────────
    let alice_members = alice.group_member_ids(&group_id).await;
    assert_eq!(alice_members.len(), 2, "the admin must still see the roster");
    let bob_members = bob.group_member_ids(&group_id).await;
    assert_eq!(bob_members.len(), 2, "a plain member must still see the roster");
    assert!(
        !alice.list_group_channels(&group_id).await.is_empty(),
        "the admin must still see the channel list"
    );
    assert!(
        !bob.list_group_channels(&group_id).await.is_empty(),
        "a plain member must still see the channel list"
    );
    let bob_emoji = bob
        .invoke_json(
            "list_group_emoji",
            json!({ "groupId": group_id, "requesterId": bob.user_id() }),
        )
        .await;
    assert_eq!(
        bob_emoji.as_array().expect("emoji array").len(),
        1,
        "a member must still see the group's emoji list"
    );

    // ── The roster ─────────────────────────────────────────────────────────
    // The headline exposure: usernames and avatar URLs for everyone in a group
    // the caller has no relationship to.
    let err = mallory
        .invoke_try(
            "get_group_members",
            json!({ "groupId": group_id, "requesterId": mallory.user_id() }),
        )
        .await
        .expect_err("a non-member must not receive a group's roster");
    assert!(
        err.contains("not a member of this group"),
        "a non-member should be told they are outside the group, got: {err}"
    );

    // ── The channel list ───────────────────────────────────────────────────
    let err = mallory
        .invoke_try(
            "list_group_channels",
            json!({ "groupId": group_id, "requesterId": mallory.user_id() }),
        )
        .await
        .expect_err("a non-member must not receive a group's channel list");
    assert!(
        err.contains("not a member of this group"),
        "a non-member should be told they are outside the group, got: {err}"
    );

    // ── The emoji list ─────────────────────────────────────────────────────
    // Listing a group's emoji is group metadata (shortcodes name things, and
    // `created_by` names members). Distinct from RESOLVING one hash, which
    // stays open — see `emoji_rendering_ignores_group_membership`.
    let err = mallory
        .invoke_try(
            "list_group_emoji",
            json!({ "groupId": group_id, "requesterId": mallory.user_id() }),
        )
        .await
        .expect_err("a non-member must not receive a group's emoji list");
    assert!(
        err.contains("not a member of this group"),
        "a non-member should be told they are outside the group, got: {err}"
    );

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// GUARD, not a proof of the fix: #848 requires that an emoji from a group you
/// are in NONE of still RENDERS when someone sends it, because the content hash
/// resolves without a membership check by design. #917 closes the emoji
/// *listing*, and the risk is that the guard is put one layer too low and takes
/// rendering with it.
///
/// The assertion is that a non-member and a member reach the SAME outcome —
/// membership is not an input to this path at all. The shared outcome is the
/// object-key mismatch `seed_group_emoji` arms deliberately, which is reached
/// after the object lookup and before any network call, so the test is hermetic
/// and its failure mode is a wrong error rather than a flake.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn emoji_rendering_ignores_group_membership() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    mallory.sign_up("mallory@test.local").await;

    let group_id = alice.create_group("Emoji Owners").await;
    let content_hash = "b".repeat(64);
    crate::harness::seed_group_emoji(&group_id, "shipit", &content_hash, &alice_profile.id).await;

    alice.arm_media_server().await;
    mallory.arm_media_server().await;

    let member_err = alice
        .invoke_try("get_emoji_url", json!({ "contentHash": content_hash }))
        .await
        .expect_err("the seeded object key is deliberately wrong");
    let outsider_err = mallory
        .invoke_try("get_emoji_url", json!({ "contentHash": content_hash }))
        .await
        .expect_err("the seeded object key is deliberately wrong");

    assert!(
        !outsider_err.contains("not a member of this group"),
        "#848: resolving an emoji hash must never consult group membership, got: {outsider_err}"
    );
    assert_eq!(
        member_err, outsider_err,
        "#848: a member and a non-member must reach the identical outcome — membership \
         is not an input to the render path. member: {member_err}, outsider: {outsider_err}"
    );
    assert!(
        outsider_err.contains("unexpected object key"),
        "the render path should have reached the object-key check, meaning it got past \
         the lookup with no membership gate in the way, got: {outsider_err}"
    );

    drop(alice);
    drop(mallory);
}

/// GUARD for the regression this fix is most likely to cause: `search_group_by_slug`
/// is how a stranger FINDS a group to join, so it deliberately keeps taking no
/// membership check. What changed is the SHAPE — it now answers with the minimal
/// public projection instead of the whole `groups` row.
///
/// Asserts the full join path end to end from OUTSIDE the group (search →
/// request → approve → member), and that the projection carries what the join
/// prompt renders and nothing more.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_non_member_can_still_find_and_join_a_group() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Joinable Group").await;

    // ── Discovery works for someone with no relationship to the group ──────
    let found = carol
        .invoke_json("search_group_by_slug", json!({ "slug": "joinable-group" }))
        .await;
    assert_eq!(found["id"].as_str(), Some(group_id.as_str()));
    assert_eq!(found["name"].as_str(), Some("Joinable Group"));

    // ── …and discloses only the join prompt's fields ───────────────────────
    // `owner_id` names a specific user and is the #917 roster leak in
    // miniature; `created_at` has no consumer. Asserted as absent KEYS, so
    // re-adding either to the returned struct fails here.
    assert!(
        found.get("owner_id").is_none(),
        "the public projection must not name the group's owner, got: {found}"
    );
    assert!(
        found.get("created_at").is_none(),
        "the public projection must not carry the group's creation time, got: {found}"
    );
    // Sorted before comparing: `serde_json`'s `Map` is a `BTreeMap` here (the
    // `preserve_order` feature is not enabled anywhere in the tree), so the
    // wire order is alphabetical, not declaration order. The property under
    // test is the SET of disclosed fields, so compare it as a set and leave
    // field ordering — which nothing depends on — unpinned.
    let mut keys: Vec<&str> = found
        .as_object()
        .expect("object")
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["description", "id", "name"],
        "the public projection is exactly the join prompt's fields"
    );

    // ── The roster is NOT reachable from a search hit ───────────────────────
    // Finding a group must not become a second door onto the thing #917 closed.
    let err = carol
        .invoke_try(
            "get_group_members",
            json!({ "groupId": group_id, "requesterId": carol.user_id() }),
        )
        .await
        .expect_err("finding a group must not confer its roster");
    assert!(err.contains("not a member of this group"), "got: {err}");

    // ── The rest of the join flow still completes ──────────────────────────
    carol.request_group_access(&group_id).await;
    let requests = alice.list_join_requests(&group_id).await;
    assert_eq!(requests.len(), 1, "the admin must see the pending request");
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

    alice.approve_join_request(&request_id).await;
    carol.poll().await;

    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&carol_profile.id), "carol should have joined");
    assert!(ids.contains(&alice_profile.id));

    // And now that she is inside, the guarded reads open up for her.
    assert!(
        !carol.list_group_channels(&group_id).await.is_empty(),
        "a newly approved member must be able to read the channel list"
    );
    assert_eq!(
        carol.group_member_ids(&group_id).await.len(),
        2,
        "a newly approved member must be able to read the roster"
    );

    drop(alice);
    drop(carol);
}
