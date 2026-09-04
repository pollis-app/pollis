//! Receipts (#857) must be **opaque to the Delivery Service**.
//!
//! The DS is untrusted. The entire feature is only correct if a server operator
//! (or anyone with a Turso dump / subpoena) cannot learn *who read what, when*.
//! That is not a property of the ciphertext alone — it is a property of every
//! artifact a receipt leaves on the server: the request line, the stored row,
//! and the columns that row is made of.
//!
//! So these tests do not check that receipts work. They try to *find* a receipt
//! from the DS's point of view, and assert it cannot be found:
//!
//! 1. There is no receipt endpoint to call. A `/v1/receipts/*` route would put
//!    "this is a receipt" in the request line — handing the DS, in plaintext,
//!    precisely the metadata the ciphertext exists to withhold. Receipts ride
//!    `/v1/messages/send` for that reason.
//! 2. A receipt and an ordinary text message produce rows that are identical in
//!    every stored column's shape — same `type`, same blinded sender, same
//!    sealed flag, same ciphertext length. The DS cannot sort its envelope
//!    stream into "messages" and "receipts".
//! 3. Nothing from the receipt's plaintext — the kind, the reader, the
//!    acknowledged message ids — survives into any stored column.
//! 4. The DS never interprets the ciphertext: it round-trips whatever bytes it
//!    is handed, so there is no server-side parse that could learn anything.

use std::sync::Arc;

use axum::http::{Request, StatusCode};
use pollis_delivery::db::Db;
use pollis_delivery::messages::{apply_send_message, SendMessageBody};
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;


/// The blinded sender every send writes under unconditional sealed sender
/// (#607) — receipts included.
const SEALED_SENTINEL: &str = "sealed";

/// The DM the receipts in these tests are exchanged in.
const CONV: &str = "dm1";

/// Message ids a receipt acknowledges. If any of these ever appear in a stored
/// column, the DS has learned which messages were read.
const ACKED: [&str; 2] = [
    "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    "01J8XQ2M5H7NR3T0V9WZ4KDCEB",
];

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO conversation (id, kind) VALUES (?1, 'dm')",
            libsql::params![CONV],
        )
        .await
        .expect("claim");
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, 'alice', '2026-01-01')",
            libsql::params![CONV],
        )
        .await
        .expect("dm");
    for user in ["alice", "bob"] {
        db.conn().await
            .unwrap()
            .execute(
                "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at) \
                 VALUES (?1, ?2, 'alice', '2026-01-01', '2026-01-01')",
                libsql::params![CONV, user],
            )
            .await
            .expect("member");
    }
    db
}

/// The v1 receipt frame (`0xF8`), rebuilt here independently of `pollis-core`.
/// Writing it out a second time is deliberate: it keeps this crate free of a
/// dependency on the client, and it means the test knows exactly which bytes
/// must never reach the server.
fn receipt_plaintext(read: bool, at: &str, ids: &[&str]) -> Vec<u8> {
    let mut buf = vec![0xF8u8, u8::from(read)];
    buf.extend_from_slice(&(at.len() as u32).to_le_bytes());
    buf.extend_from_slice(at.as_bytes());
    buf.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    for id in ids {
        buf.extend_from_slice(&(id.len() as u32).to_le_bytes());
        buf.extend_from_slice(id.as_bytes());
    }
    // Zero-pad to the same 256-byte floor bucket every short frame lands in.
    buf.resize(256, 0u8);
    buf
}

/// Stand-in for the MLS sealing the client performs before posting. The DS never
/// holds a group key, so from its side the payload is exactly this: bytes it
/// cannot invert. Deterministic so the test is reproducible.
fn seal(plaintext: &[u8]) -> String {
    let mut out = String::from("mls:");
    for (i, b) in plaintext.iter().enumerate() {
        out.push_str(&format!("{:02x}", b ^ ((i as u8).wrapping_mul(31).wrapping_add(7))));
    }
    out
}

fn body(id: &str, ciphertext: &str) -> SendMessageBody {
    SendMessageBody {
        id: id.to_string(),
        conversation_id: CONV.to_string(),
        sender_id: Some(SEALED_SENTINEL.to_string()),
        ciphertext: ciphertext.to_string(),
        reply_to_id: None,
        sent_at: "2026-08-15T12:00:00Z".to_string(),
        sealed: 1,
        generation: None,
        epoch: None,
        // No push from a unit test — this asserts envelope opacity.
        push_to: None,
    }
}

/// Everything the DS stores for one envelope, as text — the complete
/// server-side view of it.
async fn stored_row(db: &Db, id: &str) -> String {
    let mut rows = db
        .conn().await
        .unwrap()
        .query(
            "SELECT id, conversation_id, sender_id, ciphertext, \
                    COALESCE(reply_to_id,''), type, COALESCE(target_message_id,''), \
                    CAST(sealed AS TEXT), sent_at \
             FROM message_envelope WHERE id = ?1",
            libsql::params![id],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("rows").expect("row present");
    let mut out = String::new();
    for i in 0..8 {
        out.push_str(&row.get::<String>(i).unwrap_or_default());
        out.push('\u{1}');
    }
    out.push_str(&row.get::<String>(8).unwrap_or_default());
    out
}

/// A `/v1/receipts/*` endpoint — any endpoint whose very name announces the
/// payload — must not exist. This is the leak that ciphertext cannot fix: the
/// request line is plaintext to the DS, so a dedicated route would tell an
/// operator "bob acknowledged something in dm1 at 12:00" on every single read.
#[tokio::test]
async fn ds_exposes_no_receipt_endpoint() {
    let db = fresh().await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));

    for path in [
        "/v1/receipts/send",
        "/v1/receipts/add",
        "/v1/receipts",
        "/v1/messages/receipts",
        "/v1/read-receipts/send",
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let status = router.clone().oneshot(req).await.unwrap().status();
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} must not exist — a receipt-named route leaks who acknowledged what, \
             in the clear, regardless of how well the body is encrypted",
        );
    }
}

/// The DS cannot tell a receipt from an ordinary short message. Both go through
/// the real `apply_send_message`; every stored column must come out the same
/// shape, so there is no server-side predicate that isolates receipts.
#[tokio::test]
async fn receipt_is_indistinguishable_from_an_ordinary_message() {
    let db = fresh().await;

    // A read receipt acknowledging two messages...
    let receipt_ct = seal(&receipt_plaintext(true, "2026-08-15T12:00:00Z", &ACKED));
    // ...and an ordinary short text message, padded to the same bucket.
    let mut text = vec![0xF5u8];
    text.extend_from_slice(&2u32.to_le_bytes());
    text.extend_from_slice(b"hi");
    text.resize(256, 0u8);
    let text_ct = seal(&text);

    for (id, ct) in [("env_receipt", &receipt_ct), ("env_text", &text_ct)] {
        let outcome = apply_send_message(&db.conn().await.unwrap(), Some("bob"), &body(id, ct))
            .await
            .expect("send");
        assert!(matches!(outcome, WriteOutcome::Ok), "{id} must be accepted");
    }

    let receipt_row = stored_row(&db, "env_receipt").await;
    let text_row = stored_row(&db, "env_text").await;

    // Column-by-column: everything except the opaque id and the opaque
    // ciphertext must be literally equal, and those two must match in LENGTH.
    let r: Vec<&str> = receipt_row.split('\u{1}').collect();
    let t: Vec<&str> = text_row.split('\u{1}').collect();
    assert_eq!(r[2], t[2], "sender_id: both blinded to the same sentinel");
    assert_eq!(r[2], SEALED_SENTINEL);
    assert_eq!(r[3].len(), t[3].len(), "ciphertext length must not give it away");
    assert_eq!(r[4], t[4], "reply_to_id");
    assert_eq!(r[5], t[5], "type: a receipt is stored as an ordinary 'message'");
    assert_eq!(r[5], "message");
    assert_eq!(r[6], t[6], "target_message_id: empty for both");
    assert_eq!(r[7], t[7], "sealed flag");
    assert_eq!(r[8], t[8], "sent_at shape");

    // And there is no column anywhere in the table that could name a receipt.
    let mut rows = db
        .conn().await
        .unwrap()
        .query("SELECT name FROM pragma_table_info('message_envelope')", ())
        .await
        .expect("pragma");
    while let Some(row) = rows.next().await.expect("row") {
        let name: String = row.get(0).unwrap();
        assert!(
            !name.to_lowercase().contains("receipt")
                && !name.to_lowercase().contains("read")
                && !name.to_lowercase().contains("seen"),
            "message_envelope.{name} would be a server-visible receipt flag",
        );
    }
}

/// Nothing from the receipt's plaintext reaches the server. The kind, the
/// timestamp the reader stamped, and above all the ids of the messages being
/// acknowledged must not appear in any stored column.
#[tokio::test]
async fn receipt_plaintext_never_reaches_the_stored_row() {
    let db = fresh().await;
    let plaintext = receipt_plaintext(true, "2026-08-15T12:00:00Z", &ACKED);
    let ct = seal(&plaintext);

    apply_send_message(&db.conn().await.unwrap(), Some("bob"), &body("env_r", &ct))
        .await
        .expect("send");

    let row = stored_row(&db, "env_r").await;
    for acked in ACKED {
        assert!(
            !row.contains(acked),
            "the DS learned WHICH message was acknowledged ({acked}) — that is the leak",
        );
    }
    for token in ["read", "delivered", "receipt", "\u{F8}"] {
        assert!(
            !row.contains(token),
            "the token {token:?} survived into the DS's view of the envelope",
        );
    }
    // The reader's identity is not in the row either — the sender column is the
    // blinded sentinel, and the true reader lives only in the MLS credential
    // inside the ciphertext.
    assert!(!row.contains("bob"), "the reader must not be attributable from the row");

    // Sanity: the plaintext really did contain what we just proved is absent, so
    // this test cannot pass vacuously.
    let as_text = String::from_utf8_lossy(&plaintext);
    assert!(as_text.contains(ACKED[0]) && as_text.contains(ACKED[1]));
}

/// The DS never parses the ciphertext — it stores and returns whatever bytes it
/// is handed. Demonstrated by round-tripping a receipt and a deliberately
/// nonsense payload through the identical code path with identical results:
/// there is no server-side interpretation that could learn anything.
#[tokio::test]
async fn ds_round_trips_ciphertext_without_interpreting_it() {
    let db = fresh().await;
    let receipt_ct = seal(&receipt_plaintext(false, "2026-08-15T12:00:00Z", &ACKED));
    let garbage_ct = "mls:deadbeefdeadbeef";

    for (id, ct) in [("env_a", receipt_ct.as_str()), ("env_b", garbage_ct)] {
        let outcome = apply_send_message(&db.conn().await.unwrap(), Some("bob"), &body(id, ct))
            .await
            .expect("send");
        assert!(
            matches!(outcome, WriteOutcome::Ok),
            "the DS must accept {id} without inspecting its payload",
        );
        let mut rows = db
            .conn().await
            .unwrap()
            .query(
                "SELECT ciphertext FROM message_envelope WHERE id = ?1",
                libsql::params![id],
            )
            .await
            .expect("query");
        let stored: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(stored, ct, "{id} must round-trip byte-for-byte");
    }
}

/// Membership authz is unchanged for receipts — they are still ordinary
/// envelope writes, so a non-member cannot inject one. Sealing blinds the stored
/// sender; it never widens who may write.
#[tokio::test]
async fn a_non_member_cannot_post_a_receipt() {
    let db = fresh().await;
    let ct = seal(&receipt_plaintext(true, "2026-08-15T12:00:00Z", &ACKED));
    let outcome = apply_send_message(&db.conn().await.unwrap(), Some("mallory"), &body("env_x", &ct))
        .await
        .expect("send");
    assert!(
        matches!(outcome, WriteOutcome::Forbidden),
        "a non-member must not be able to write a receipt envelope",
    );
}
