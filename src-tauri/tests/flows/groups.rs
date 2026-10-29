use crate::harness::{await_wake_up, enroll_second_device, wipe, TestClient};
use serde_json::json;
use serial_test::serial;

/// Minimal end-to-end path: sign up, create a group, list it back.
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

    // Keep the client (and its local DB handle) alive past the assertions.
    drop(alice);
}

/// Two clients: isolated keystores, and cross-client username search.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn two_clients_see_each_other_via_turso() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    assert_ne!(alice_profile.id, bob_profile.id);

    let hit: serde_json::Value = bob
        .invoke_json(
            "search_user_by_username",
            json!({ "username": alice_profile.username.clone() }),
        )
        .await;
    assert_eq!(hit["id"], alice_profile.id);
    assert_eq!(hit["username"], alice_profile.username);

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

/// Invite, accept, then remove.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_invite_accept_then_remove_flow() {
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

    assert!(bob.first_pending_invite().await.is_none());

    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&alice_profile.id));
    assert!(ids.contains(&bob_profile.id));
    assert_eq!(ids.len(), 2);

    assert!(bob.list_group_ids().await.contains(&group_id));

    alice.remove_member(&group_id, &bob_profile.id).await;

    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id.clone()]);
    assert!(!bob.list_group_ids().await.contains(&group_id));

    drop(alice);
    drop(bob);
}

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

    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id]);
    assert!(!bob.list_group_ids().await.contains(&group_id));
    let _ = bob_profile;

    drop(alice);
    drop(bob);
}

/// #1506: pending invites are listed; revoking one also removes the invitee's devices from the tree.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_revoked_invite_leaves_the_list_and_the_tree() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Revoke Test").await;
    alice.invite(&group_id, &carol_profile.username).await;
    let carol_invite = carol.first_pending_invite().await.expect("carol's invite");
    carol
        .accept_invite(carol_invite["id"].as_str().expect("invite id"))
        .await;
    carol.poll().await;

    alice.invite(&group_id, &bob_profile.username).await;
    let in_tree = |members: &[(String, String)], user: &str| members.iter().any(|(u, _)| u == user);
    let tree = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        in_tree(&tree, &bob_profile.id),
        "precondition: a pending invitee is grafted into the tree at invite time"
    );

    let listed = carol.list_group_invites(&group_id).await;
    assert_eq!(listed.len(), 1, "carol should see bob's pending invite: {listed:?}");
    let invite = &listed[0];
    assert_eq!(invite["invitee_id"], bob_profile.id);
    assert_eq!(invite["invitee_username"], bob_profile.username);
    assert_eq!(invite["inviter_id"], alice_profile.id);
    assert!(invite["created_at"].as_str().is_some_and(|s| !s.is_empty()));
    let invite_id = invite["id"].as_str().expect("invite id").to_string();

    assert!(
        carol
            .try_revoke_invite(&group_id, &invite_id)
            .await
            .is_err(),
        "a plain member must not revoke someone else's invite"
    );
    assert_eq!(alice.list_group_invites(&group_id).await.len(), 1);

    let epoch_before = crate::harness::ds_head_epoch(&group_id).await;
    alice
        .try_revoke_invite(&group_id, &invite_id)
        .await
        .expect("the inviter may revoke");

    assert!(alice.list_group_invites(&group_id).await.is_empty());
    assert!(bob.first_pending_invite().await.is_none(), "bob's invite is gone");
    let tree = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        !in_tree(&tree, &bob_profile.id),
        "a revoked invitee must be committed out of the tree, got {tree:?}"
    );
    assert!(in_tree(&tree, &carol_profile.id), "carol stays in the tree");
    assert!(
        crate::harness::ds_head_epoch(&group_id).await > epoch_before,
        "the removal is a published commit"
    );

    let channel_id = alice.general_channel_id(&group_id).await;
    carol.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "after the revoke").await;
    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    assert!(
        carol_msgs.iter().any(|m| m["content"] == "after the revoke"),
        "carol must still read the group after the revoke: {carol_msgs:#?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// The mobile `membership` flow's sequence (#1506).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_new_member_reads_the_creator_after_an_invite_and_a_promotion() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Promote Test").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, "bob@test.local").await;
    let invite = bob.first_pending_invite().await.expect("bob's invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    bob.send_channel_message(&channel_id, "hello from the phone").await;

    alice.send_channel_message(&channel_id, "hello from the admin").await;
    alice.invite(&group_id, &carol_profile.username).await;
    alice.set_member_role(&group_id, &bob_profile.id, "admin").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    assert!(
        msgs.iter().any(|m| m["content"] == "hello from the admin"),
        "bob must read alice's post: {msgs:#?}"
    );

    let invites = bob.list_group_invites(&group_id).await;
    assert_eq!(invites.len(), 1, "{invites:?}");
    let invite_id = invites[0]["id"].as_str().expect("invite id").to_string();
    bob.try_revoke_invite(&group_id, &invite_id)
        .await
        .expect("an admin may revoke someone else's invite");
    assert!(alice.list_group_invites(&group_id).await.is_empty());

    drop(alice);
    drop(bob);
    drop(carol);
}

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

/// Roster, channels and emoji require membership (#917); admin reads distinguish non-member from non-admin (#875).
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

    // A plain member, so the allowed case includes a non-admin.
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

    // Listing is gated; resolving one hash stays open (`emoji_rendering_ignores_group_membership`).
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

    let mallory_links = mallory
        .invoke_try(
            "list_group_invite_links",
            json!({ "groupId": group_id, "userId": mallory.user_id() }),
        )
        .await
        .expect_err("a non-member must not receive an invite-link list");
    assert!(
        mallory_links.contains("not a member of this group"),
        "a non-member should be told they are outside the group, got: {mallory_links}"
    );

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// GUARD (#848): a non-member and a member reach the SAME outcome when rendering an emoji.
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

/// GUARD: `search_group_by_slug` stays open to strangers but returns only the join prompt's fields.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_non_member_can_still_find_and_join_a_group() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Joinable Group").await;

    let found = carol
        .invoke_json("search_group_by_slug", json!({ "slug": "joinable-group" }))
        .await;
    assert_eq!(found["id"].as_str(), Some(group_id.as_str()));
    assert_eq!(found["name"].as_str(), Some("Joinable Group"));

    // Asserted as absent keys, so re-adding either fails here.
    assert!(
        found.get("owner_id").is_none(),
        "the public projection must not name the group's owner, got: {found}"
    );
    assert!(
        found.get("created_at").is_none(),
        "the public projection must not carry the group's creation time, got: {found}"
    );
    // `Map` is a `BTreeMap` here, so compare as a set.
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

    // A search hit must not open the roster (#917).
    let err = carol
        .invoke_try(
            "get_group_members",
            json!({ "groupId": group_id, "requesterId": carol.user_id() }),
        )
        .await
        .expect_err("finding a group must not confer its roster");
    assert!(err.contains("not a member of this group"), "got: {err}");

    carol.request_group_access(&group_id).await;
    let requests = alice.list_join_requests(&group_id).await;
    assert_eq!(requests.len(), 1, "the admin must see the pending request");
    assert_eq!(requests[0]["requester_id"], carol_profile.id);
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

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

    assert!(alice.list_join_requests(&group_id).await.is_empty());

    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&carol_profile.id), "carol should have joined");
    assert!(ids.contains(&alice_profile.id));
    assert_eq!(ids.len(), 2);

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

/// Without media, group-room wake-ups go through the DS's SendData.
#[cfg(not(feature = "media"))]
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_structure_wake_ups_leave_a_core_without_a_room() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let group = alice.create_group("Structure").await;
    alice.invite(&group, &bob_profile.username).await;
    let invite = bob.first_pending_invite().await.expect("bob's invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;

    let is = |kind: &'static str| {
        let group = group.clone();
        move |p: &serde_json::Value| p["type"] == kind && p["group_id"] == group.as_str()
    };

    alice.create_channel(&group, "fresh").await;
    let wake = await_wake_up(&group, is("membership_changed"))
        .await
        .expect("create_channel must wake the group's room");
    assert!(!(400..500).contains(&wake.status), "refused: {wake:?}");

    alice.set_member_role(&group, &bob_profile.id, "admin").await;
    let wake = await_wake_up(&group, is("member_role_changed"))
        .await
        .expect("set_member_role must wake the group's room");
    assert!(!(400..500).contains(&wake.status), "refused: {wake:?}");

    drop(alice);
    drop(bob);
}

/// A new group reaches the creator's other devices via a wake-up on the account inbox.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_group_created_on_one_device_wakes_the_users_other_devices() {
    wipe().await;

    let mut d1 = TestClient::new().await;
    let me = d1.sign_up("alice@test.local").await;
    let d2 = enroll_second_device(&d1, "alice@test.local").await;
    assert!(d2.list_group_ids().await.is_empty(), "d2 starts with no groups");

    let group = d1.create_group("Made on d1").await;

    // No LiveKit here: accepted = 503 "not configured", refused = 4xx.
    let inbox = format!("inbox-{}", me.id);
    let wake = await_wake_up(&inbox, |p| {
        p["type"] == "membership_changed" && p["group_id"] == group.as_str()
    })
    .await
    .unwrap_or_else(|| panic!("create_group must wake the user's other devices on {inbox}"));
    assert!(!(400..500).contains(&wake.status), "the DS refused the wake-up: {wake:?}");
    // No `kind`, so no invite alert for the user's own group.
    assert!(wake.payload.get("kind").map_or(true, |k| k.is_null()), "{wake:?}");

    d2.poll().await;
    assert!(
        d2.list_group_ids().await.contains(&group),
        "d2 must list the group d1 created"
    );
    let channel = d1.general_channel_id(&group).await;
    d1.send_channel_message(&channel, "made on d1").await;
    d2.invoke_json(
        "ingest_channel_envelopes",
        json!({ "userId": d2.user_id(), "channelId": channel }),
    )
    .await;
    let page = d2
        .invoke_json("read_channel_messages", json!({ "channelId": channel, "limit": 50 }))
        .await;
    let got: Vec<&str> = page["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(got.contains(&"made on d1"), "d2 must read d1's message, got {got:?}");

    drop(d1);
    drop(d2);
}
