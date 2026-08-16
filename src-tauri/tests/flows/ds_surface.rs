//! #918 / #875 — the DS surface the suite actually runs against.
//!
//! Two things used to be true at once and nobody noticed: the harness declared
//! its own router, and that router was missing ten routes. So a client call that
//! 404'd under test looked exactly like a client call that worked, and two
//! shipped features (`/v1/invite-links/*`, custom emoji #848) were "covered" by
//! a suite that could not reach them.
//!
//! The harness now mounts the real `pollis-delivery` router (see
//! `harness::spawn_in_process_delivery`), which removes the mirror entirely. The
//! tests here keep it removed:
//!
//!   * [`every_declared_endpoint_is_reachable`] walks `pollis_api::ENDPOINTS` —
//!     the single declaration both the client and the router are built from — and
//!     requires the in-process DS to serve each one. The mirror of this test
//!     lives in `pollis-delivery/tests/endpoint_coverage.rs`; this one proves it
//!     for the binding the *integration suite* talks to.
//!   * The emoji and invite-link scenarios are the coverage that was missing,
//!     driven as signed requests from a real enrolled client.

use crate::harness::{
    delivery_url, raw_post_status, signed_post_status, wipe, writable_remote, TestClient,
};
use pollis_api::DsRequest;
use serde_json::json;
use serial_test::serial;

/// Every endpoint the client can address must resolve to a handler on the DS the
/// suite runs against.
///
/// The probe is UNSIGNED and the body is `{}`, so essentially every endpoint
/// answers 401 (auth is enforced in the harness) or 400. Both prove a handler
/// ran. Only 404 proves none did — which is precisely the failure that hid
/// `/v1/invite-links/*` and custom emoji from this suite.
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

/// Control for the test above: the probe can see a route that is genuinely
/// absent. Without this, `every_declared_endpoint_is_reachable` would pass just
/// as happily if `raw_post_status` never returned 404 at all.
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

/// #848 custom emoji, end to end through the DS — the feature that shipped with
/// zero integration coverage because the harness had no `/v1/emoji/*` route.
///
/// The upload command itself cannot run here: it presigns and PUTs to R2, and the
/// test DS has no R2 credentials. What it does that R2 cannot fake is the part
/// that was broken — the client's request body reaching the right handler and the
/// write landing — so the body posted is the very
/// [`pollis_api::emoji::CreateEmojiBody`] `upload_group_emoji` builds, signed by a
/// real enrolled device.
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

    // The write landed, and it landed as BOTH rows: the per-group binding and the
    // deduped object. Asserting only the 200 would pass against a handler that
    // returns OK and writes nothing.
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

    // Remove releases the binding.
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

/// The authorization half of the same endpoint: a validly-signed NON-member is
/// refused. 403 (identity proved, permission denied) rather than 401, which is
/// what makes this an authz assertion instead of an auth one.
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

/// `/v1/invite-links/redeem` — the third route #918 left off the harness, and the
/// only one of the three invite-link endpoints that a joining (non-member) user
/// calls, so its absence was invisible to every member-side test.
///
/// Redeeming a token that does not exist must be a clean refusal from the
/// HANDLER, never a 404 from the router. The two are indistinguishable to a
/// client, which is exactly how this stayed hidden.
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
