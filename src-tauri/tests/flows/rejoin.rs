use crate::harness::{wipe, TestClient};
use serial_test::serial;

/// Regression guard for the cross-signing-verification misroute (group_state.rs).
///
/// Manual repro this mirrors: A creates a group + channel, invites B, B accepts,
/// B is promoted to admin, B leaves, then B rejoins. After B rejoins and sends a
/// message, A could not see it (and vice versa).
///
/// Root cause it exercises: when B is re-added, the STAYING member A must catch
/// up to that re-add commit via `process_pending_commits`, which runs
/// cross-signing cert verification (`verify_added_devices`). That verification
/// reads `users` / `user_device` / `account_key_log` — all MAIN-DB tables. The
/// fix gave it a dedicated `state.remote_db` connection; before the fix it ran on
/// the `log_db` connection, which in the two-DB harness has NO `users` table, so
/// the verify errors ("no such table: users"), the error is swallowed as
/// `AbsentRetry`, and A defers the re-add commit — stranding A at the old epoch
/// so it cannot decrypt B's post-rejoin message.
///
/// The verification is best-effort/swallowed, so the ONLY thing that catches the
/// regression is asserting that the catching-up member can read a message sent
/// at the new (post-rejoin) epoch. That is the load-bearing assertion below.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn member_rejoin_messages_visible_to_stayer() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Rejoin").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // ── B joins the first time ──
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have a pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Sanity: the round-trip works before any churn.
    alice.send_channel_message(&channel_id, "before-leave").await;
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"before-leave"),
        "bob should decrypt alice's message before leaving, got: {bob_contents:?}"
    );

    // ── B is promoted to admin (mirrors the manual repro) ──
    alice
        .set_member_role(&group_id, bob.user_id(), "admin")
        .await;

    // ── B leaves ──
    bob.leave_group(&group_id).await;
    // A reconciles B's departure into her own tree.
    alice.process_commits_for(&channel_id).await;

    // ── B rejoins (re-invite + accept) ──
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have a pending re-invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    // Both sides catch up to the re-add commit. A's catch-up here is the one
    // that runs the cross-signing verify on the re-add — the buggy path.
    bob.process_commits_for(&channel_id).await;
    alice.process_commits_for(&channel_id).await;

    // Confirm B is genuinely a current member again.
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob_profile.id),
        "bob must be a current member after rejoining, got: {members:?}"
    );

    // ── LOAD-BEARING ASSERTION: B sends after rejoining; the stayer A must
    //    read it. On the buggy (misrouted-verify) tree A deferred the re-add
    //    commit, stayed at the old epoch, and cannot decrypt this. ──
    bob.send_channel_message(&channel_id, "rejoin-from-bob").await;
    alice.process_commits_for(&channel_id).await;
    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    let alice_contents: Vec<&str> =
        alice_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        alice_contents.contains(&"rejoin-from-bob"),
        "STAYER A could not read rejoiner B's post-rejoin message — the re-add \
         commit was deferred (cross-signing verify misrouted to the log DB). \
         got: {alice_msgs:#?}"
    );

    // ── Reverse direction: A sends after the rejoin; B must read it. ──
    alice.send_channel_message(&channel_id, "rejoin-from-alice").await;
    bob.process_commits_for(&channel_id).await;
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"rejoin-from-alice"),
        "rejoiner B could not read stayer A's post-rejoin message, got: {bob_contents:?}"
    );

    drop(alice);
    drop(bob);
}
