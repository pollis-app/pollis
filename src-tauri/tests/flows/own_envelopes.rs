//! Each fetched envelope is opened at most once, and a real failure is still reported (#1629).
//!
//! Before: every device tried to open its own receipts and edits when the fetch returned them
//! ("not an application message"), and overlapping catch-ups opened the same envelopes twice
//! ("secret reuse"). Both logged an error for an envelope nothing was wrong with.

use std::collections::HashMap;

use crate::harness::{expect_losses, wipe, writable_remote, TestClient};
use pollis_core::commands::messages::{
    decrypt_attempts, forget_decrypt_attempts, logged_decrypt_failures,
};
use pollis_core::commands::mls::DecryptFailureKind;
use serial_test::serial;

/// Every envelope id the DS holds for `conversation_id`.
async fn envelope_ids(conversation_id: &str) -> Vec<String> {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT id FROM message_envelope WHERE conversation_id = ?1 ORDER BY seq",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("envelope ids");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        out.push(row.get::<String>(0).expect("id"));
    }
    out
}

/// What was logged for these envelopes, by id.
fn logged_for(ids: &[String]) -> HashMap<String, DecryptFailureKind> {
    logged_decrypt_failures()
        .into_iter()
        .filter(|(id, _)| ids.contains(id))
        .collect()
}

/// Alice and Bob with an accepted DM both hold.
async fn dm_pair() -> (TestClient, TestClient, String) {
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dm_id = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm_id).await;
    alice.fetch_dm_messages(&dm_id).await;
    bob.fetch_dm_messages(&dm_id).await;
    (alice, bob, dm_id)
}

/// A device's own messages, receipts and edits come back on its fetch; it never tries to open
/// them, so nothing is logged. Each envelope is opened once, by the one device it is for.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn own_envelopes_are_skipped_without_a_decrypt() {
    wipe().await;
    forget_decrypt_attempts();
    let (alice, bob, dm_id) = dm_pair().await;

    let mine = alice.send_channel_message_id(&dm_id, "from alice").await;
    bob.send_channel_message(&dm_id, "from bob").await;
    // Each fetch emits delivered and read-side receipts, which come back to their sender.
    for _ in 0..2 {
        alice.fetch_dm_messages(&dm_id).await;
        bob.fetch_dm_messages(&dm_id).await;
    }
    alice.edit_message(&dm_id, &mine, "from alice (edited)").await;
    for _ in 0..2 {
        alice.fetch_dm_messages(&dm_id).await;
        bob.fetch_dm_messages(&dm_id).await;
    }

    let ids = envelope_ids(&dm_id).await;
    assert!(ids.len() >= 5, "messages, receipts and an edit: {ids:?}");
    assert!(
        logged_for(&ids).is_empty(),
        "an envelope that opened fine on the device it was for was reported as failing: {:?}",
        logged_for(&ids)
    );
    let attempts = decrypt_attempts();
    for id in &ids {
        let n = attempts.get(id).copied().unwrap_or(0);
        assert!(
            n <= 1,
            "envelope {id} reached a decrypt {n} times: its sender tried to open its own, \
             or one device opened it twice"
        );
    }
    let bob_view = bob.fetch_dm_messages(&dm_id).await;
    assert!(
        bob_view.iter().any(|m| m["content"] == "from alice (edited)"),
        "the edit still applies: {bob_view:#?}"
    );
}

/// Catch-ups of one group that overlap open each envelope once.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn overlapping_catch_ups_open_each_envelope_once() {
    wipe().await;
    forget_decrypt_attempts();
    let (alice, bob, dm_id) = dm_pair().await;

    for round in 0..3 {
        let sent = bob.send_channel_message_id(&dm_id, &format!("burst {round}")).await;
        bob.edit_message(&dm_id, &sent, &format!("burst {round} (edited)")).await;
        // A wake-driven ingest racing a send's catch-up and a page load, as on desktop.
        let (a, b, c) = tokio::join!(
            alice.fetch_dm_messages(&dm_id),
            alice.fetch_dm_messages(&dm_id),
            alice.fetch_dm_messages(&dm_id),
        );
        for page in [&a, &b, &c] {
            assert!(page.iter().all(|m| m["undecryptable"].as_bool() != Some(true)), "{page:#?}");
        }
    }

    let ids = envelope_ids(&dm_id).await;
    let reused: Vec<_> = logged_for(&ids)
        .into_iter()
        .filter(|(_, kind)| *kind == DecryptFailureKind::SecretReuse)
        .collect();
    assert!(reused.is_empty(), "an overlapping catch-up opened an envelope twice: {reused:?}");
    let attempts = decrypt_attempts();
    for id in &ids {
        let n = attempts.get(id).copied().unwrap_or(0);
        assert!(n <= 1, "envelope {id} reached a decrypt {n} times");
    }
    let page = alice.fetch_dm_messages(&dm_id).await;
    for round in 0..3 {
        let want = format!("burst {round} (edited)");
        assert!(page.iter().any(|m| m["content"] == want.as_str()), "missing {want}: {page:#?}");
    }
}

/// A key reused under an envelope this device never opened is a sender fault: still reported.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_reused_key_on_a_new_envelope_is_still_reported() {
    wipe().await;
    forget_decrypt_attempts();
    let (alice, bob, dm_id) = dm_pair().await;

    let original = bob.send_channel_message_id(&dm_id, "once").await;
    alice.fetch_dm_messages(&dm_id).await;

    // Bob's ciphertext again under a fresh id and the next position: a forked sender ratchet.
    let forged = format!("{}FORK", &original[..original.len() - 4]);
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut cols = Vec::new();
    let mut rows = conn
        .query("SELECT name FROM pragma_table_info('message_envelope')", ())
        .await
        .expect("columns");
    while let Some(row) = rows.next().await.expect("row") {
        let name = row.get::<String>(0).expect("name");
        if name != "id" && name != "seq" {
            cols.push(name);
        }
    }
    let list = cols.join(", ");
    conn.execute(
        &format!(
            "INSERT INTO message_envelope (id, seq, {list}) \
             SELECT ?1, (SELECT MAX(seq) + 1 FROM message_envelope), {list} \
             FROM message_envelope WHERE id = ?2"
        ),
        libsql::params![forged.clone(), original.clone()],
    )
    .await
    .expect("replay the envelope under a new id");

    // The replay cannot open, so it shows as a lost message: that is the point.
    expect_losses();
    alice.fetch_dm_messages(&dm_id).await;

    let logged = logged_for(std::slice::from_ref(&forged));
    assert_eq!(
        logged.get(&forged),
        Some(&DecryptFailureKind::SecretReuse),
        "a reused key on an envelope this device never opened must be reported: {logged:?}"
    );
}
