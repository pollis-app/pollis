//! Delivery-cursor poisoning. Drives the real `pollis_delivery` write handlers
//! against a local libsql DB, exactly as the DS runs them in production.
//!
//! `message_envelope.sent_at` and `conversation_watermark.last_fetched_at` are
//! the delivery cursor: ingest fetches `sent_at > last_fetched_at`, the
//! watermark is monotone and never rewinds, and envelope GC deletes
//! `sent_at < MIN(last_fetched_at)`. Both values are chosen by the CLIENT, and
//! until the bound these tests pin the DS stored them verbatim. Two attacks
//! followed:
//!
//!   * **A member posts `sent_at = "9999-…"`.** Every recipient fetches it (it
//!     sorts above every cursor), reports a cursor of `9999-…`, and from then on
//!     receives nothing in that conversation — and once every live device has
//!     reported, the next GC sweep deletes every envelope, fetched or not.
//!   * **A non-member forges a watermark.** `advance_watermark` had no
//!     membership check and took `device_id` from the body. GC joins the real
//!     roster and ignored the row, but the tombstone floor did not: the next
//!     admin delete was stamped `9999-…000000001`, every device that applied it
//!     adopted that as its cursor, and the same blackout + GC followed —
//!     triggered from OUTSIDE the conversation.
//!
//! Neither is one of the three losses `CLAUDE.md` permits. The fix is one
//! admission rule at the chokepoint (`check_cursor_stamp`: canonical UTC
//! RFC 3339, no further ahead of the DS clock than the signature window), a
//! membership + signing-device gate on `advance_watermark`, and a floor that
//! reads only the member-device roster. Each test here fails against the
//! pre-fix code.

use pollis_delivery::db::Db;
use pollis_delivery::messages::{
    apply_advance_watermark, apply_delete_message, apply_edit_message, apply_envelope_gc,
    apply_send_message, DeleteMessageBody, EditMessageBody, EnvelopeGcBody, SendMessageBody,
    WatermarkBody, CURSOR_STAMP_SKEW_SECS,
};
use pollis_delivery::writes::WriteOutcome;

mod common;

const POISON: &str = "9999-12-31T23:59:59.000000000+00:00";

/// A staleness window wide enough that nothing here is excluded for dormancy —
/// these tests are about the cursor value, not device liveness (#720).
const STALE: &str = "-6 months";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    // Channel `c1` of group `g1`: alice (admin, device a1) and bob (device b1).
    // mallory has an account and a device but is NOT a member.
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

/// A client stamp `minutes` from the DS's now (negative = past).
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
    }
}

fn watermark(user: &str, device: &str, at: &str) -> WatermarkBody {
    WatermarkBody {
        conversation_id: "c1".to_string(),
        user_id: Some(user.to_string()),
        device_id: device.to_string(),
        last_fetched_at: at.to_string(),
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

/// `(user_id, device_id, last_fetched_at)` for every watermark row of `c1`.
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

// ── (1) a far-future `sent_at` is refused, on send and on edit ───────────────

/// **The `sent_at` poisoning regression test.** bob (a member, authenticated)
/// posts an otherwise well-formed envelope stamped in year 9999. It must not be
/// stored: stored, it is the cursor every recipient adopts, after which
/// `sent_at > cursor` matches nothing for them ever again.
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

    // The same value one skew-window past now is refused too — the bound is the
    // signature window, not "this millennium".
    let just_past = (chrono::Utc::now() + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS + 60))
        .to_rfc3339();
    let out = apply_send_message(&conn, Some("bob"), &send("m-ahead", &just_past))
        .await
        .unwrap();
    assert!(is_invalid(&out), "{just_past} must be refused, got {out:?}");

    // …while an honest client clock slightly ahead of the DS is admitted, as is
    // every stamp at or behind it.
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

/// Every envelope-inserting path with a client stamp is bounded: an edit with a
/// poisoned `sent_at` is refused, AND — because the check runs before the
/// transaction — the author's pending edit it would have replaced survives.
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
    // A malformed stamp is refused on the same path.
    let out = apply_edit_message(&conn, Some("alice"), &edit("e-bad", "m1", "not-a-time"))
        .await
        .unwrap();
    assert!(is_invalid(&out), "{out:?}");
}

// ── (2) `advance_watermark` is gated on membership, device and value ─────────

/// A non-member cannot write a watermark row under a conversation, however
/// honest the value. Pre-fix this was a 200 and a stored row.
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

/// The device half is bound to the signing device: a member naming another
/// device — its own second device, or someone else's — is refused, and the row
/// that lands carries the SIGNED device id.
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

/// A member cannot park its own cursor in the far future either: the value is
/// bounded regardless of who reports it. Pre-fix the monotone `MAX` adopted it
/// and nothing could ever bring it back.
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
        // SQLite's shape — parses as a date to a human, sorts below every RFC
        // 3339 stamp (#908); a cursor in it would silently mean "consumed nothing".
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

// ── (3) the tombstone floor reads only the member-device roster ──────────────

/// **The tombstone-floor poisoning regression test.** A `9999-…` watermark row
/// exists for `c1` under a non-member (written directly — the endpoint refuses
/// it now, so this is the defence-in-depth layer). alice admin-deletes bob's
/// message. The tombstone must be stamped at wall-clock now, not one nanosecond
/// past year 9999: every device applying it adopts its `sent_at` as its cursor.
#[tokio::test]
async fn a_non_members_watermark_row_does_not_floor_the_admin_tombstone() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    assert!(matches!(
        apply_send_message(&conn, Some("bob"), &send("m1", &client_stamp(-10))).await.unwrap(),
        WriteOutcome::Ok
    ));
    // Both real member devices have fetched m1.
    for (u, d) in [("alice", "a1"), ("bob", "b1")] {
        assert!(matches!(
            apply_advance_watermark(&conn, Some((u, d)), &watermark(u, d, &client_stamp(-10)))
                .await
                .unwrap(),
            WriteOutcome::Ok
        ));
    }
    // The forged row, planted around the endpoint.
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

    // And the sweep-side consequence the attack aimed for cannot follow: with
    // the honest devices' cursors behind the tombstone, GC keeps it.
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
