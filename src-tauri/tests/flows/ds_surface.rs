//! #918 / #875: the harness mounts the real `pollis-delivery` router; these keep every route reachable.

use crate::harness::{
    delivery_url, raw_post_status, signed_post_status, wipe, writable_remote, TestClient,
};
use pollis_api::DsRequest;
use serde_json::json;
use serial_test::serial;

/// Unsigned `{}` probes get 401/400 (a handler ran); only 404 means none did.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn every_declared_endpoint_is_reachable() {
    wipe().await;
    let base = delivery_url().await;

    let mut missing = Vec::new();
    for endpoint in pollis_api::ENDPOINTS {
        let status = raw_post_status(
            &base,
            endpoint.path,
            &[("Content-Type", "application/json")],
            b"{}",
        )
        .await;
        if status == 404 {
            missing.push(format!("{} ({})", endpoint.path, endpoint.type_name));
        }
    }

    assert!(
        missing.is_empty(),
        "the flows suite cannot reach these endpoints, so nothing it asserts about \
         them means anything:\n  {}",
        missing.join("\n  ")
    );
}

/// Control: the probe really can see an absent route.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_reachability_probe_can_see_a_missing_route() {
    wipe().await;
    let base = delivery_url().await;
    let status = raw_post_status(
        &base,
        "/v1/definitely-not-an-endpoint",
        &[("Content-Type", "application/json")],
        b"{}",
    )
    .await;
    assert_eq!(
        status, 404,
        "an unrouted path must probe as 404, or the coverage test above is vacuous"
    );
}

/// #848 custom emoji through the DS, with the exact body `upload_group_emoji` builds (no R2 here).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn custom_emoji_create_and_remove_round_trip_through_the_ds() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group: serde_json::Value = alice
        .invoke_json(
            "create_group",
            json!({ "name": "Emoji Group", "description": null, "ownerId": alice.user_id() }),
        )
        .await;
    let group_id = group["id"].as_str().expect("group id").to_string();

    let content_hash = "a".repeat(64);
    let create = pollis_api::emoji::CreateEmojiBody {
        group_id: group_id.clone(),
        shortcode: "parrot".into(),
        content_hash: content_hash.clone(),
        content_type: "image/webp".into(),
        size_bytes: 1024,
        animated: false,
        created_by: Some(alice.user_id().to_string()),
    };
    let status = signed_post_status(
        &alice,
        pollis_api::emoji::CreateEmojiBody::PATH,
        &serde_json::to_vec(&create).expect("serialize create"),
    )
    .await;
    assert_eq!(
        status, 200,
        "a group admin's emoji create must be accepted; before #918 this path 404'd \
         in the suite and nothing here was ever exercised"
    );

    // Both rows: the per-group binding and the deduped object.
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let count = |sql: &'static str, p: Vec<String>| {
        let conn = conn.clone();
        async move {
            let mut rows = conn.query(sql, p).await.expect("query");
            let row = rows.next().await.expect("rows").expect("one row");
            row.get::<i64>(0).expect("count")
        }
    };
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM group_emoji WHERE group_id = ?1 AND shortcode = ?2",
            vec![group_id.clone(), "parrot".to_string()],
        )
        .await,
        1,
        "the group_emoji binding must exist after a 200"
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM custom_emoji_object WHERE content_hash = ?1",
            vec![content_hash.clone()],
        )
        .await,
        1,
        "the deduped object row must exist after a 200"
    );

    let remove = pollis_api::emoji::RemoveEmojiBody {
        group_id: group_id.clone(),
        shortcode: "parrot".into(),
        actor_id: Some(alice.user_id().to_string()),
    };
    let status = signed_post_status(
        &alice,
        pollis_api::emoji::RemoveEmojiBody::PATH,
        &serde_json::to_vec(&remove).expect("serialize remove"),
    )
    .await;
    assert_eq!(status, 200, "the admin's emoji remove must be accepted");
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM group_emoji WHERE group_id = ?1 AND shortcode = ?2",
            vec![group_id.clone(), "parrot".to_string()],
        )
        .await,
        0,
        "remove must actually delete the binding"
    );

    drop(alice);
}

/// A signed non-member gets 403, not 401: authz, not auth.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_non_member_cannot_add_an_emoji_to_someone_elses_group() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    mallory.sign_up("mallory@test.local").await;

    let group: serde_json::Value = alice
        .invoke_json(
            "create_group",
            json!({ "name": "Alice's Group", "description": null, "ownerId": alice.user_id() }),
        )
        .await;
    let group_id = group["id"].as_str().expect("group id").to_string();

    let create = pollis_api::emoji::CreateEmojiBody {
        group_id,
        shortcode: "sneaky".into(),
        content_hash: "b".repeat(64),
        content_type: "image/webp".into(),
        size_bytes: 1024,
        animated: false,
        created_by: Some(mallory.user_id().to_string()),
    };
    let status = signed_post_status(
        &mallory,
        pollis_api::emoji::CreateEmojiBody::PATH,
        &serde_json::to_vec(&create).expect("serialize"),
    )
    .await;
    assert_eq!(
        status, 403,
        "a non-member must be refused on permission, not merely unauthenticated"
    );

    drop((alice, mallory));
}

/// #1280: a member may add an emoji but not remove it, even their own; an admin may.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_member_can_add_an_emoji_but_only_an_admin_can_remove_it() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Emoji Commons").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite = bob.first_pending_invite().await.expect("bob's invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;

    let create = pollis_api::emoji::CreateEmojiBody {
        group_id: group_id.clone(),
        shortcode: "bobs_parrot".into(),
        content_hash: "c".repeat(64),
        content_type: "image/webp".into(),
        size_bytes: 1024,
        animated: false,
        created_by: Some(bob.user_id().to_string()),
    };
    let status = signed_post_status(
        &bob,
        pollis_api::emoji::CreateEmojiBody::PATH,
        &serde_json::to_vec(&create).expect("serialize create"),
    )
    .await;
    assert_eq!(status, 200, "a plain member must be able to add an emoji");

    let remove_as = |actor: &TestClient| pollis_api::emoji::RemoveEmojiBody {
        group_id: group_id.clone(),
        shortcode: "bobs_parrot".into(),
        actor_id: Some(actor.user_id().to_string()),
    };
    let status = signed_post_status(
        &bob,
        pollis_api::emoji::RemoveEmojiBody::PATH,
        &serde_json::to_vec(&remove_as(&bob)).expect("serialize remove"),
    )
    .await;
    assert_eq!(
        status, 403,
        "a plain member must not be able to remove an emoji, even one they added"
    );

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let bindings = || {
        let conn = conn.clone();
        let group_id = group_id.clone();
        async move {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM group_emoji WHERE group_id = ?1 AND shortcode = 'bobs_parrot'",
                    libsql::params![group_id],
                )
                .await
                .expect("query");
            let row = rows.next().await.expect("rows").expect("one row");
            row.get::<i64>(0).expect("count")
        }
    };
    assert_eq!(bindings().await, 1, "the refused remove must leave the binding");

    let status = signed_post_status(
        &alice,
        pollis_api::emoji::RemoveEmojiBody::PATH,
        &serde_json::to_vec(&remove_as(&alice)).expect("serialize remove"),
    )
    .await;
    assert_eq!(status, 200, "the admin may remove a member's emoji");
    assert_eq!(bindings().await, 0, "the admin's remove must delete the binding");

    drop((alice, bob));
}

/// Redeeming an unknown token is a handler refusal, never a router 404 (#918).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn redeeming_an_unknown_invite_link_is_refused_by_the_handler_not_the_router() {
    wipe().await;

    let mut bob = TestClient::new().await;
    bob.sign_up("bob@test.local").await;

    let body = pollis_api::groups::RedeemInviteLinkBody {
        token: "not-a-real-token".into(),
        user_id: Some(bob.user_id().to_string()),
        attempt_id: "01JXXXXXXXXXXXXXXXXXXXXXXX".into(),
        now: "2026-01-01T00:00:00Z".into(),
    };
    let status = signed_post_status(
        &bob,
        pollis_api::groups::RedeemInviteLinkBody::PATH,
        &serde_json::to_vec(&body).expect("serialize"),
    )
    .await;
    assert_ne!(
        status, 404,
        "the redeem route must exist; a 404 here is the router, not the token"
    );
    assert!(
        (400..500).contains(&status),
        "an unknown token must be refused (4xx), got {status}"
    );

    drop(bob);
}
