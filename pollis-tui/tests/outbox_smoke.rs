//! The durable outbox from the CLI (#1335): failed sends are kept and marked, and `R` / `x`
//! reach the apps' core commands.
//!
//! Its own binary: it flips the process-wide "client outdated" switch.

mod common;

use std::sync::atomic::Ordering;

use common::{spawn_world, TestClient};
use pollis_core::commands::messages::ChannelMessage;

async fn row(client: &TestClient, dm_id: &str, content: &str) -> Option<ChannelMessage> {
    client
        .read_dm(dm_id)
        .await
        .messages
        .into_iter()
        .find(|m| m.content.as_deref() == Some(content))
}

async fn failing_send(client: &TestClient, dm_id: &str, content: &str) {
    client.activate();
    let username = client.profile.as_ref().map(|p| p.username.clone());
    let outdated = client.state.update_required.clone();
    outdated.store(true, Ordering::SeqCst);
    let sent = pollis_tui::send::send_text(&client.state, client.user_id(), username, dm_id, content).await;
    outdated.store(false, Ordering::SeqCst);
    assert!(sent.is_err(), "an outdated client cannot send");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_send_is_marked_and_the_cli_can_retry_or_discard_it() {
    let world = spawn_world().await;
    let mut alice = TestClient::new(&world);
    let mut bob = TestClient::new(&world);
    alice.sign_up("outbox-cli-alice@test.local").await;
    let bob_profile = bob.sign_up("outbox-cli-bob@test.local").await;
    let dm_id = alice.create_dm(&bob_profile.id).await;
    bob.accept_dm(&dm_id).await;

    // Two sends fail. Both are kept, and the timeline marks both as not sent.
    failing_send(&alice, &dm_id, "retry me").await;
    failing_send(&alice, &dm_id, "discard me").await;
    let keep = row(&alice, &dm_id, "retry me").await.expect("a failed send is kept");
    let toss = row(&alice, &dm_id, "discard me").await.expect("a failed send is kept");
    assert_eq!(keep.send_state.as_deref(), Some("failed"));
    assert!(pollis_tui::timeline::is_unsent_failure(&keep));
    assert!(pollis_tui::timeline::is_unsent_failure(&toss));

    // `R`: retried under the same id, delivered once.
    alice.activate();
    pollis_tui::send::retry(&alice.state, &keep.id).await.expect("retry");
    let delivered = row(&alice, &dm_id, "retry me").await.expect("still there");
    assert_eq!(delivered.id, keep.id);
    assert_eq!(delivered.send_state, None);
    assert!(!pollis_tui::timeline::is_unsent_failure(&delivered));

    // `x` on an unsent row: discarded, and it reached nobody.
    alice.activate();
    assert!(pollis_tui::send::discard(&alice.state, &toss.id).await.expect("discard"));
    assert!(row(&alice, &dm_id, "discard me").await.is_none());

    bob.sync_rounds(4).await;
    let theirs: Vec<String> = bob
        .read_dm(&dm_id)
        .await
        .messages
        .into_iter()
        .filter_map(|m| m.content)
        .collect();
    assert_eq!(theirs.iter().filter(|c| *c == "retry me").count(), 1, "{theirs:?}");
    assert!(!theirs.iter().any(|c| c == "discard me"), "{theirs:?}");
}
