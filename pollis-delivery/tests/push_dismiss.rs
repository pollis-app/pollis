//! #1508: a read clears its notifications on every device.
//!
//! The rules themselves — only the reader's own handles, only for the
//! conversations read, only those minted before the read, only once — and the
//! silent push's shape and coalescing are pinned next to the code in
//! `src/push.rs`. This file drives the two endpoints through the real router,
//! so the wire bodies, the routing and the request caps are what a client
//! actually meets.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pollis_api::devices::{
    DismissEntry, DismissPushBody, DismissPushResponse, DismissedPushHandlesBody,
    DismissedPushHandlesResponse, DISMISSED_QUERY_MAX_HANDLES, DISMISS_MAX_CONVERSATIONS,
};
use pollis_api::DsRequest;
use pollis_delivery::push::mint_push_handles;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn fixture() -> (axum::Router, common::TempDb) {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    // Auth off: the actor is the body's `user_id`, exactly as a signed request
    // whose signer matches it. Owner scoping is what is under test here; the
    // signature gate is common to every device-signed endpoint.
    (build_router_with_state(AppState::new(db.arc(), false)), db)
}

async fn post<B: DsRequest + serde::Serialize>(
    app: &axum::Router,
    body: &B,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(B::PATH)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn dismiss(user: &str, conversations: &[(&str, u32)]) -> DismissPushBody {
    DismissPushBody {
        user_id: user.into(),
        conversations: conversations
            .iter()
            .map(|(c, age)| DismissEntry {
                conversation_id: (*c).into(),
                read_secs_ago: *age,
            })
            .collect(),
    }
}

#[tokio::test]
async fn reading_a_conversation_dismisses_its_notifications_once() {
    let (app, db) = fixture().await;
    let conn = db.conn().await.unwrap();
    let alice = vec!["alice".to_string()];
    let h1 = mint_push_handles(&conn, &alice, "conv-1", "dm").await["alice"].clone();
    let h2 = mint_push_handles(&conn, &alice, "conv-2", "channel").await["alice"].clone();
    // A second ago at the latest, so "read 0 seconds ago" is after the mint.
    conn.execute("UPDATE push_handle SET created_at = datetime('now', '-2 seconds')", ())
        .await
        .unwrap();

    // Someone else naming alice's conversation dismisses nothing of hers.
    let (status, json) = post(&app, &dismiss("mallory", &[("conv-1", 0)])).await;
    assert_eq!(status, StatusCode::OK);
    let resp: DismissPushResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.dismissed, 0);

    let (status, json) = post(&app, &dismiss("alice", &[("conv-1", 0)])).await;
    assert_eq!(status, StatusCode::OK);
    let resp: DismissPushResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.dismissed, 1);

    // Again: nothing outstanding, so nothing to push.
    let (_, json) = post(&app, &dismiss("alice", &[("conv-1", 0)])).await;
    assert_eq!(serde_json::from_value::<DismissPushResponse>(json).unwrap().dismissed, 0);

    let query = DismissedPushHandlesBody {
        user_id: "alice".into(),
        handles: vec![h1.clone(), h2.clone(), "never-minted".into()],
    };
    let (status, json) = post(&app, &query).await;
    assert_eq!(status, StatusCode::OK);
    let resp: DismissedPushHandlesResponse = serde_json::from_value(json).unwrap();
    assert_eq!(resp.dismissed, vec![h1.clone()], "only the read conversation's");

    // Another account cannot learn that alice's handle exists or was read.
    let peek = DismissedPushHandlesBody {
        user_id: "mallory".into(),
        handles: vec![h1],
    };
    let (_, json) = post(&app, &peek).await;
    let resp: DismissedPushHandlesResponse = serde_json::from_value(json).unwrap();
    assert!(resp.dismissed.is_empty());
}

/// A message that arrived after the read keeps its notification.
#[tokio::test]
async fn a_notification_newer_than_the_read_survives() {
    let (app, db) = fixture().await;
    let conn = db.conn().await.unwrap();
    let h = mint_push_handles(&conn, &["alice".to_string()], "conv-1", "dm").await["alice"].clone();

    // Read a minute ago, reported only now (a slow or offline client).
    let (_, json) = post(&app, &dismiss("alice", &[("conv-1", 60)])).await;
    assert_eq!(serde_json::from_value::<DismissPushResponse>(json).unwrap().dismissed, 0);
    let (_, json) = post(
        &app,
        &DismissedPushHandlesBody {
            user_id: "alice".into(),
            handles: vec![h],
        },
    )
    .await;
    assert!(serde_json::from_value::<DismissedPushHandlesResponse>(json)
        .unwrap()
        .dismissed
        .is_empty());
}

#[tokio::test]
async fn oversized_requests_are_refused() {
    let (app, _db) = fixture().await;
    let many: Vec<(String, u32)> = (0..=DISMISS_MAX_CONVERSATIONS)
        .map(|i| (format!("c{i}"), 0))
        .collect();
    let refs: Vec<(&str, u32)> = many.iter().map(|(c, a)| (c.as_str(), *a)).collect();
    let (status, _) = post(&app, &dismiss("alice", &refs)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let query = DismissedPushHandlesBody {
        user_id: "alice".into(),
        handles: (0..=DISMISSED_QUERY_MAX_HANDLES).map(|i| format!("h{i}")).collect(),
    };
    let (status, _) = post(&app, &query).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// An older client never calls these; a newer client against an older DS
/// gets a 404 it treats as best-effort. Pin that the bodies stay tolerant of
/// a missing field in the answer, so a future response change cannot break a
/// shipped client's parse.
#[test]
fn responses_parse_from_an_empty_object() {
    assert_eq!(
        serde_json::from_str::<DismissPushResponse>("{}").unwrap(),
        DismissPushResponse::default()
    );
    assert_eq!(
        serde_json::from_str::<DismissedPushHandlesResponse>("{}").unwrap(),
        DismissedPushHandlesResponse::default()
    );
}
