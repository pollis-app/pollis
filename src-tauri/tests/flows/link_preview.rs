//! Sender-generated link previews (#1987) through the real send, ingest and
//! edit paths: the preview reaches the recipient inside the ciphertext, its
//! thumbnail is a counted reference released with the message, and an edit
//! rebuilds it.

use crate::harness::{wipe, writable_remote, TestClient};
use pollis_core::commands::messages::test_hooks;
use serde_json::json;
use serial_test::serial;

/// A 1x1 PNG: enough for the thumbnail re-encode.
const PNG_1X1: [u8; 70] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0,
    0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2, 135,
    1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

struct StubbedUploads;

impl StubbedUploads {
    fn on() -> Self {
        test_hooks::stub_attachment_uploads(true);
        StubbedUploads
    }
}

impl Drop for StubbedUploads {
    fn drop(&mut self) {
        test_hooks::stub_attachment_uploads(false);
    }
}

async fn pair() -> (TestClient, TestClient, String) {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("preview-alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("preview-bob@test.local").await;
    let group_id = alice.create_group("Previews").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite = bob.first_pending_invite().await.expect("bob has an invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    (alice, bob, channel_id)
}

fn draft(url: &str, title: &str, with_image: bool) -> serde_json::Value {
    let image = with_image.then(|| {
        let staged = pollis_core::commands::staging::stage_attachment(PNG_1X1.to_vec()).expect("stage");
        json!({ "stagedId": staged.id, "width": 1, "height": 1 })
    });
    json!({ "url": url, "title": title, "description": "A description", "image": image })
}

async fn send(client: &TestClient, channel_id: &str, content: &str, preview: serde_json::Value) -> String {
    let msg = client
        .invoke_json(
            "send_message",
            json!({
                "conversationId": channel_id,
                "senderId": client.user_id(),
                "content": content,
                "replyToId": null,
                "threadId": null,
                "linkPreview": preview,
            }),
        )
        .await;
    msg["id"].as_str().expect("message id").to_string()
}

async fn edit(client: &TestClient, channel_id: &str, id: &str, content: &str, preview: serde_json::Value) {
    client
        .invoke_json(
            "edit_message",
            json!({
                "conversationId": channel_id,
                "messageId": id,
                "userId": client.user_id(),
                "newContent": content,
                "linkPreview": preview,
            }),
        )
        .await;
}

async fn row(client: &TestClient, channel_id: &str, id: &str) -> serde_json::Value {
    client
        .fetch_channel_messages(channel_id)
        .await
        .into_iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("no row {id}"))
}

async fn refs(message_id: &str, hash: &str) -> i64 {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM attachment_ref WHERE content_hash = ?1 AND message_id = ?2",
            libsql::params![hash.to_string(), message_id.to_string()],
        )
        .await
        .expect("attachment_ref query");
    rows.next().await.expect("row").expect("some row").get(0).expect("count")
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_preview_reaches_the_recipient_and_an_edit_rebuilds_it() {
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();

    let id = send(
        &alice,
        &channel_id,
        "have a look https://example.com/a",
        draft("https://example.com/a", "Example A", true),
    )
    .await;

    let got = row(&bob, &channel_id, &id).await;
    assert_eq!(got["content"], "have a look https://example.com/a", "the text is unchanged");
    let preview = &got["link_preview"];
    assert_eq!(preview["url"], "https://example.com/a");
    assert_eq!(preview["title"], "Example A");
    assert_eq!(preview["description"], "A description");
    let hash = preview["image"]["hash"].as_str().expect("the thumbnail arrived").to_string();
    assert_eq!(preview["image"]["key"], format!("media/{hash}.enc"));
    assert_eq!(preview["image"]["ct"], "image/jpeg");
    assert_eq!(row(&alice, &channel_id, &id).await["link_preview"], *preview, "the sender's copy matches");
    assert_eq!(refs(&id, &hash).await, 1, "the thumbnail is a counted reference of the message");

    // An edit rebuilds the message: a new link, a new preview without an image.
    edit(&alice, &channel_id, &id, "actually https://example.org/b", draft("https://example.org/b", "Example B", false)).await;
    let edited = row(&bob, &channel_id, &id).await;
    assert_eq!(edited["link_preview"]["title"], "Example B");
    assert!(edited["link_preview"]["image"].is_null());
    assert_eq!(refs(&id, &hash).await, 0, "the replaced thumbnail is released");

    // An edit with no preview leaves the message with none.
    edit(&alice, &channel_id, &id, "no link any more", serde_json::Value::Null).await;
    assert!(row(&bob, &channel_id, &id).await["link_preview"].is_null());

    drop(alice);
    drop(bob);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_preview_of_a_link_not_in_the_text_is_never_sent() {
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();
    let id = send(&alice, &channel_id, "no links here", draft("https://example.com/a", "Spoof", true)).await;
    assert!(row(&bob, &channel_id, &id).await["link_preview"].is_null());
    assert!(row(&alice, &channel_id, &id).await["link_preview"].is_null());
    drop(alice);
    drop(bob);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn deleting_a_message_takes_its_preview_and_thumbnail() {
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();
    let id = send(&alice, &channel_id, "https://example.com/a", draft("https://example.com/a", "A", true)).await;
    let hash = row(&bob, &channel_id, &id).await["link_preview"]["image"]["hash"]
        .as_str()
        .expect("thumbnail")
        .to_string();
    alice.delete_message(&id).await;
    assert!(row(&alice, &channel_id, &id).await["link_preview"].is_null());
    assert!(row(&bob, &channel_id, &id).await["link_preview"].is_null());
    assert_eq!(refs(&id, &hash).await, 0);
    drop(alice);
    drop(bob);
}
