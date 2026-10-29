//! Delivery-cursor poisoning, against the real write handlers and a local libsql DB.

use pollis_delivery::db::Db;
use pollis_delivery::messages::{
    apply_advance_watermark, apply_delete_message, apply_edit_message, apply_envelope_gc,
    apply_send_message, DeleteMessageBody, EditMessageBody, EnvelopeGcBody, SendMessageBody,
    WatermarkBody, CURSOR_STAMP_SKEW_SECS,
};
use pollis_delivery::writes::WriteOutcome;

/// A capability hash with known preimage (#1135): `base64(SHA-256("fixture-capability"))`.
const FIXTURE_CAPABILITY_HASH: &str = "MB8lyVjsEQamFuSzLTRZBVKGFQQ8nXoBjHFF7kvNuAo=";
const FIXTURE_CAPABILITY_TOKEN: &str = "fixture-capability";

mod common;

const POISON: &str = "9999-12-31T23:59:59.000000000+00:00";

/// The DS default window (#1486): nothing here is excluded for dormancy.
const STALE: &str = pollis_delivery::messages::DEFAULT_WATERMARK_STALE_MODIFIER;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    // Channel `c1` of group `g1`: alice (a1) and bob (b1); mallory is not a member.
    conn.execute_batch(
        "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
         INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin');
         INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');
         INSERT INTO user_device (user_id, device_id) VALUES ('alice', 'a1');
         INSERT INTO user_device (user_id, device_id) VALUES ('bob', 'b1');
         INSERT INTO user_device (user_id, device_id) VALUES ('mallory', 'm1');",
    )
    .await
    .unwrap();
    db
}

fn client_stamp(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339()
}

fn send(id: &str, sent_at: &str) -> SendMessageBody {
    SendMessageBody {
        id: id.to_string(),
        conversation_id: "c1".to_string(),
        sender_id: Some("sealed".to_string()),
        ciphertext: "mls:00".to_string(),
        reply_to_id: None,
        sent_at: sent_at.to_string(),
        sealed: 1,
        generation: None,
        epoch: None,
        push_to: None,
        push_mentions: None,
        delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
    }
}

fn edit(envelope_id: &str, target: &str, sent_at: &str) -> EditMessageBody {
    EditMessageBody {
        envelope_id: envelope_id.to_string(),
        conversation_id: "c1".to_string(),
        target_message_id: target.to_string(),
        sender_id: None,
        ciphertext: "mls:01".to_string(),
        sent_at: sent_at.to_string(),
        generation: None,
        epoch: None,
        // The target has a capability (#1135), so the token must be presented.
        delete_token: Some(FIXTURE_CAPABILITY_TOKEN.to_string()),
    }
}

fn watermark(user: &str, device: &str, at: &str) -> WatermarkBody {
    WatermarkBody {
        conversation_id: "c1".to_string(),
        user_id: Some(user.to_string()),
        device_id: device.to_string(),
        last_fetched_at: at.to_string(),
        last_seq: None,
    }
}

async fn envelope_sent_ats(db: &Db) -> Vec<String> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT sent_at FROM message_envelope WHERE conversation_id = 'c1' ORDER BY sent_at",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

async fn envelope_ids(db: &Db) -> Vec<String> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT id FROM message_envelope WHERE conversation_id = 'c1' ORDER BY id",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

async fn watermark_rows(db: &Db) -> Vec<(String, String, String)> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT user_id, device_id, last_fetched_at FROM conversation_watermark \
             WHERE conversation_id = 'c1' ORDER BY user_id, device_id",
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push((
            r.get::<String>(0).unwrap(),
            r.get::<String>(1).unwrap(),
            r.get::<String>(2).unwrap(),
        ));
    }
    out
}

fn is_invalid(outcome: &WriteOutcome) -> bool {
    matches!(outcome, WriteOutcome::Invalid(_))
}

/// A far-future `sent_at` must not be stored: every recipient would adopt it as cursor.
#[tokio::test]
async fn a_member_cannot_store_an_envelope_stamped_in_the_far_future() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let out = apply_send_message(&conn, Some("bob"), &send("m-poison", POISON))
        .await
        .unwrap();
    assert!(is_invalid(&out), "a 9999-… sent_at must be refused, got {out:?}");
    assert!(
        envelope_ids(&db).await.is_empty(),
        "nothing may be stored for a refused send"
    );

    // The bound is the signature window, not "this millennium".
    let just_past = (chrono::Utc::now() + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS + 60))
        .to_rfc3339();
    let out = apply_send_message(&conn, Some("bob"), &send("m-ahead", &just_past))
        .await
        .unwrap();
    assert!(is_invalid(&out), "{just_past} must be refused, got {out:?}");

    let slightly_ahead = client_stamp(1);
    let out = apply_send_message(&conn, Some("bob"), &send("m-ok-ahead", &slightly_ahead))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
    let behind = client_stamp(-30);
    let out = apply_send_message(&conn, Some("bob"), &send("m-ok", &behind))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
    assert_eq!(
        envelope_sent_ats(&db).await,
        vec![behind, slightly_ahead],
        "only the admitted stamps are stored, and stored verbatim"
    );
}

/// The check runs before the transaction, so the pending edit survives.
#[tokio::test]
async fn an_edit_stamped_in_the_far_future_is_refused_and_replaces_nothing() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let honest = client_stamp(-10);
    assert!(matches!(
        apply_send_message(&conn, Some("bob"), &send("m1", &honest)).await.unwrap(),
        WriteOutcome::Ok
    ));
    let pending = client_stamp(-5);
    assert!(matches!(
        apply_edit_message(&conn, Some("bob"), &edit("e1", "m1", &pending)).await.unwrap(),
        WriteOutcome::Ok
    ));

    let out = apply_edit_message(&conn, Some("alice"), &edit("e-poison", "m1", POISON))
        .await
        .unwrap();
    assert!(is_invalid(&out), "a 9999-… edit must be refused, got {out:?}");

    assert_eq!(
        envelope_ids(&db).await,
        vec!["e1".to_string(), "m1".to_string()],
        "the refused edit stored nothing and bob's pending edit is still there"
    );
    let out = apply_edit_message(&conn, Some("alice"), &edit("e-bad", "m1", "not-a-time"))
        .await
        .unwrap();
    assert!(is_invalid(&out), "{out:?}");
}

#[tokio::test]
async fn a_non_member_cannot_advance_a_watermark() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_advance_watermark(
        &conn,
        Some(("mallory", "m1")),
        &watermark("mallory", "m1", &client_stamp(-1)),
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
    assert!(watermark_rows(&db).await.is_empty(), "no row for a non-member");
}

/// The row carries the signed device id; naming another device is refused.
#[tokio::test]
async fn the_watermark_device_is_bound_to_the_signing_device() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let at = client_stamp(-1);

    let out = apply_advance_watermark(&conn, Some(("bob", "b1")), &watermark("bob", "b2", &at))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
    let out = apply_advance_watermark(&conn, Some(("bob", "b1")), &watermark("bob", "a1", &at))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
    assert!(watermark_rows(&db).await.is_empty());

    let out = apply_advance_watermark(&conn, Some(("bob", "b1")), &watermark("bob", "b1", &at))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
    assert_eq!(
        watermark_rows(&db).await,
        vec![("bob".to_string(), "b1".to_string(), at)]
    );
}

/// A member's own far-future cursor is bounded too: the monotone `MAX` never rewinds.
#[tokio::test]
async fn a_far_future_cursor_is_refused_even_from_a_member() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let honest = client_stamp(-1);
    assert!(matches!(
        apply_advance_watermark(&conn, Some(("bob", "b1")), &watermark("bob", "b1", &honest))
            .await
            .unwrap(),
        WriteOutcome::Ok
    ));

    for bad in [
        POISON.to_string(),
        (chrono::Utc::now() + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS + 60)).to_rfc3339(),
        // SQLite's shape sorts below every RFC 3339 stamp (#908).
        "2026-01-01 00:00:00".to_string(),
    ] {
        let out = apply_advance_watermark(&conn, Some(("bob", "b1")), &watermark("bob", "b1", &bad))
            .await
            .unwrap();
        assert!(is_invalid(&out), "{bad:?} must be refused, got {out:?}");
    }
    assert_eq!(
        watermark_rows(&db).await,
        vec![("bob".to_string(), "b1".to_string(), honest)],
        "the cursor stays where the honest report left it"
    );
}

/// A forged `9999-…` row must not lift the tombstone stamp past wall-clock now.
#[tokio::test]
async fn a_non_members_watermark_row_does_not_floor_the_admin_tombstone() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    assert!(matches!(
        apply_send_message(&conn, Some("bob"), &send("m1", &client_stamp(-10))).await.unwrap(),
        WriteOutcome::Ok
    ));
    for (u, d) in [("alice", "a1"), ("bob", "b1")] {
        assert!(matches!(
            apply_advance_watermark(&conn, Some((u, d)), &watermark(u, d, &client_stamp(-10)))
                .await
                .unwrap(),
            WriteOutcome::Ok
        ));
    }
    conn.execute(
        "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
         VALUES ('c1', 'mallory', 'm1', ?1)",
        libsql::params![POISON.to_string()],
    )
    .await
    .unwrap();

    let before = chrono::Utc::now();
    let out = apply_delete_message(
        &conn,
        Some("alice"),
        &DeleteMessageBody {
            message_id: "m1".into(),
            conversation_id: "c1".into(),
            msg_sender_id: Some("bob".into()),
            actor_id: None,
            delete_token: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
    let after = chrono::Utc::now();

    let stamps = envelope_sent_ats(&db).await;
    assert_eq!(stamps.len(), 1, "the original is gone and one tombstone remains");
    let tombstone = chrono::DateTime::parse_from_rfc3339(&stamps[0])
        .expect("tombstone sent_at must be RFC3339")
        .to_utc();
    assert!(
        tombstone >= before && tombstone <= after,
        "the tombstone must be stamped at wall-clock now ({before} ..= {after}), not \
         floored by the non-member's poisoned row; got {}",
        stamps[0]
    );

    // With honest cursors behind the tombstone, GC keeps it.
    let out = apply_envelope_gc(
        &conn,
        Some("alice"),
        &EnvelopeGcBody {
            conversation_id: "c1".into(),
            is_dm: false,
            actor_id: None,
        },
        STALE,
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
    assert_eq!(
        envelope_ids(&db).await.len(),
        1,
        "the tombstone is retained until every member device has fetched past it"
    );
}
