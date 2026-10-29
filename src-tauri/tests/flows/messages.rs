use std::sync::Arc;

use crate::harness::{enroll_second_device, wipe, writable_remote, TestClient};
use serde_json::json;
use pollis_lib::commands::auth::UserProfile;
use serial_test::serial;

/// Grow 1 → 9 members and shrink back, checking who can decrypt after every change.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn epoch_ratchet_nine_users_add_remove() {
    wipe().await;

    // Index 0 is the creator and stays; 1..9 join in order and leave in reverse.
    let mut clients: Vec<TestClient> = Vec::with_capacity(9);
    let mut profiles: Vec<UserProfile> = Vec::with_capacity(9);
    for i in 0..9 {
        let mut c = TestClient::new().await;
        let p = c.sign_up(&format!("user{}@test.local", i + 1)).await;
        profiles.push(p);
        clients.push(c);
    }

    let group_id = clients[0].create_group("Ratchet").await;
    let channel_id = clients[0].general_channel_id(&group_id).await;

    for n in 1..9 {
        let invitee_username = profiles[n].username.clone();
        clients[0].invite(&group_id, &invitee_username).await;

        let invite = clients[n]
            .first_pending_invite()
            .await
            .unwrap_or_else(|| panic!("user{} should have pending invite", n + 1));
        let invite_id = invite["id"].as_str().expect("invite id").to_string();
        clients[n].accept_invite(&invite_id).await;
        clients[n].poll().await;

        for k in 0..n {
            clients[k].process_commits_for(&channel_id).await;
        }

        let label = format!("msg-{n}");
        clients[0].send_channel_message(&channel_id, &label).await;

        for k in 0..=n {
            let msgs = clients[k].fetch_channel_messages(&channel_id).await;
            let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
            assert!(
                contents.contains(&label.as_str()),
                "user{} should decrypt '{}' at growth step {}, got: {:?}",
                k + 1,
                label,
                n,
                contents
            );
        }

        // Welcomes carry no history.
        if n >= 2 {
            let newcomer_msgs = clients[n].fetch_channel_messages(&channel_id).await;
            for prior in 1..n {
                let prior_label = format!("msg-{prior}");
                let hit = newcomer_msgs
                    .iter()
                    .any(|m| m["content"].as_str() == Some(prior_label.as_str()));
                assert!(
                    !hit,
                    "user{} should NOT decrypt pre-join message '{}'",
                    n + 1,
                    prior_label
                );
            }
        }
    }

    // Shrink: remove 9..2; remaining members decrypt, the removed one cannot.
    for n in (1..9).rev() {
        let target_id = profiles[n].id.clone();
        clients[0].remove_member(&group_id, &target_id).await;

        for k in 0..n {
            clients[k].process_commits_for(&channel_id).await;
        }

        let label = format!("post-remove-{}", n + 1);
        clients[0].send_channel_message(&channel_id, &label).await;

        for k in 0..n {
            let msgs = clients[k].fetch_channel_messages(&channel_id).await;
            let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
            assert!(
                contents.contains(&label.as_str()),
                "user{} should decrypt '{}' after removing user{}, got: {:?}",
                k + 1,
                label,
                n + 1,
                contents
            );
        }

        let removed_msgs = clients[n].fetch_channel_messages(&channel_id).await;
        let removed_contents: Vec<&str> = removed_msgs
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(
            !removed_contents.contains(&label.as_str()),
            "removed user{} should NOT decrypt '{}', got: {:?}",
            n + 1,
            label,
            removed_contents
        );
    }

    clients[0]
        .send_channel_message(&channel_id, "alone-again")
        .await;
    let msgs = clients[0].fetch_channel_messages(&channel_id).await;
    let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        contents.contains(&"alone-again"),
        "creator alone should decrypt 'alone-again', got: {contents:?}"
    );

    drop(clients);
}

/// Seeded xorshift64, so `rand` feature changes can't re-seed this scenario.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen_range(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next_u64() as usize) % n
    }
}

/// Seeded churn across membership and role changes; the op log prints on failure.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn random_admin_and_membership_churn() {
    wipe().await;

    const NUM_OTHERS: usize = 5;
    const MAX_GROUP_SIZE: usize = 5;
    const NUM_OPS: usize = 30;
    const SEED: u64 = 0xC0FFEE_BEEF_F00D;

    let mut clients: Vec<TestClient> = Vec::with_capacity(NUM_OTHERS + 1);
    let mut profiles: Vec<UserProfile> = Vec::with_capacity(NUM_OTHERS + 1);
    for i in 0..=NUM_OTHERS {
        let mut c = TestClient::new().await;
        let p = c.sign_up(&format!("churn{}@test.local", i)).await;
        profiles.push(p);
        clients.push(c);
    }

    let group_id = clients[0].create_group("Churn").await;
    let channel_id = clients[0].general_channel_id(&group_id).await;

    use std::collections::BTreeSet;
    let mut members: BTreeSet<usize> = BTreeSet::from([0]);
    let mut admins: BTreeSet<usize> = BTreeSet::from([0]);

    let mut op_log: Vec<String> = Vec::new();
    let mut msg_counter: u32 = 0;

    let mut rng = XorShift64::new(SEED);

    for op_idx in 0..NUM_OPS {
        let pick = rng.gen_range(6);
        let op_name: &'static str = match pick {
            0 => "add",
            1 => "remove",
            2 => "promote",
            3 => "demote",
            4 => "send_from_admin",
            _ => "send_from_member",
        };

        let non_members: Vec<usize> = (1..=NUM_OTHERS).filter(|i| !members.contains(i)).collect();
        let removable: Vec<usize> = members.iter().copied().filter(|&i| i != 0).collect();
        let promotable: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !admins.contains(i))
            .collect();
        let demotable: Vec<usize> = admins.iter().copied().filter(|&i| i != 0).collect();
        let current_admins: Vec<usize> = admins.iter().copied().collect();
        let current_non_admin_members: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !admins.contains(i))
            .collect();

        match op_name {
            "add" => {
                if members.len() >= MAX_GROUP_SIZE || non_members.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip add (size={})", members.len()));
                    continue;
                }
                let idx = non_members[rng.gen_range(non_members.len())];
                let invitee_username = profiles[idx].username.clone();
                clients[0].invite(&group_id, &invitee_username).await;
                let invite = clients[idx]
                    .first_pending_invite()
                    .await
                    .unwrap_or_else(|| {
                        eprintln!("op_log so far: {op_log:#?}");
                        panic!("op {op_idx}: user{idx} should have a pending invite")
                    });
                let invite_id = invite["id"].as_str().expect("invite id").to_string();
                clients[idx].accept_invite(&invite_id).await;
                clients[idx].poll().await;

                for &k in members.iter() {
                    clients[k].process_commits_for(&channel_id).await;
                }
                // Later commits could have landed after the Welcome.
                clients[idx].process_commits_for(&channel_id).await;

                members.insert(idx);
                op_log.push(format!("{op_idx:02}: add user{idx}"));
            }
            "remove" => {
                if removable.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip remove (no removable)"));
                    continue;
                }
                let idx = removable[rng.gen_range(removable.len())];
                clients[0].remove_member(&group_id, profiles[idx].id.as_str()).await;

                members.remove(&idx);
                admins.remove(&idx);

                for &k in members.iter() {
                    clients[k].process_commits_for(&channel_id).await;
                }
                op_log.push(format!("{op_idx:02}: remove user{idx}"));
            }
            "promote" => {
                if promotable.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip promote (no promotable)"));
                    continue;
                }
                let idx = promotable[rng.gen_range(promotable.len())];
                clients[0]
                    .set_member_role(&group_id, profiles[idx].id.as_str(), "admin")
                    .await;
                admins.insert(idx);
                op_log.push(format!("{op_idx:02}: promote user{idx}"));
            }
            "demote" => {
                if demotable.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip demote (no demotable)"));
                    continue;
                }
                let idx = demotable[rng.gen_range(demotable.len())];
                clients[0]
                    .set_member_role(&group_id, profiles[idx].id.as_str(), "member")
                    .await;
                admins.remove(&idx);
                op_log.push(format!("{op_idx:02}: demote user{idx}"));
            }
            "send_from_admin" => {
                if current_admins.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip send_from_admin (impossible)"));
                    continue;
                }
                let idx = current_admins[rng.gen_range(current_admins.len())];
                msg_counter += 1;
                let label = format!("op{op_idx:02}-admin{idx}-m{msg_counter}");
                clients[idx].send_channel_message(&channel_id, &label).await;
                verify_message_visibility(
                    &clients, &members, &label, op_idx, &op_log, &channel_id,
                )
                .await;
                op_log.push(format!("{op_idx:02}: send_from_admin user{idx} -> {label}"));
            }
            "send_from_member" => {
                if current_non_admin_members.is_empty() {
                    op_log.push(format!(
                        "{op_idx:02}: skip send_from_member (no non-admin members)"
                    ));
                    continue;
                }
                let idx = current_non_admin_members
                    [rng.gen_range(current_non_admin_members.len())];
                msg_counter += 1;
                let label = format!("op{op_idx:02}-member{idx}-m{msg_counter}");
                clients[idx].send_channel_message(&channel_id, &label).await;
                verify_message_visibility(
                    &clients, &members, &label, op_idx, &op_log, &channel_id,
                )
                .await;
                op_log.push(format!("{op_idx:02}: send_from_member user{idx} -> {label}"));
            }
            _ => unreachable!(),
        }

        let observed = clients[0].group_member_roles(&group_id).await;
        let observed_ids: BTreeSet<String> =
            observed.iter().map(|(id, _)| id.clone()).collect();
        let observed_admin_ids: BTreeSet<String> = observed
            .iter()
            .filter(|(_, r)| r == "admin")
            .map(|(id, _)| id.clone())
            .collect();

        let expected_ids: BTreeSet<String> =
            members.iter().map(|&i| profiles[i].id.clone()).collect();
        let expected_admin_ids: BTreeSet<String> =
            admins.iter().map(|&i| profiles[i].id.clone()).collect();

        if observed_ids != expected_ids {
            eprintln!("op log: {op_log:#?}");
            panic!(
                "op {op_idx} ({op_name}): group membership mismatch\n  expected: {expected_ids:?}\n  observed: {observed_ids:?}"
            );
        }
        if observed_admin_ids != expected_admin_ids {
            eprintln!("op log: {op_log:#?}");
            panic!(
                "op {op_idx} ({op_name}): admin set mismatch\n  expected: {expected_admin_ids:?}\n  observed: {observed_admin_ids:?}"
            );
        }
    }

    eprintln!("churn op log ({} ops):", op_log.len());
    for line in &op_log {
        eprintln!("  {line}");
    }

    drop(clients);
}

/// Every current member decrypts; no non-member sees the plaintext.
async fn verify_message_visibility(
    clients: &[TestClient],
    members: &std::collections::BTreeSet<usize>,
    label: &str,
    op_idx: usize,
    op_log: &[String],
    channel_id: &str,
) {
    for k in 0..clients.len() {
        let msgs = clients[k].fetch_channel_messages(channel_id).await;
        let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        let has = contents.contains(&label);
        if members.contains(&k) {
            if !has {
                eprintln!("op log: {op_log:#?}");
                panic!(
                    "op {op_idx}: member user{k} should decrypt '{label}', got: {contents:?}"
                );
            }
        } else if has {
            eprintln!("op log: {op_log:#?}");
            panic!(
                "op {op_idx}: non-member user{k} should NOT decrypt '{label}', got: {contents:?}"
            );
        }
    }
}

/// `edit_message` across adds and removes; a stale edit can never win (DELETE+INSERT).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn edit_message_across_membership_changes() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Edit Churn").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;

    let msg_id = alice
        .send_channel_message_id(&channel_id, "hello")
        .await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"hello"),
        "bob should decrypt 'hello' before carol is added, got: {bob_contents:?}"
    );

    alice.invite(&group_id, &carol_profile.username).await;
    let invite_id = carol
        .first_pending_invite()
        .await
        .expect("carol invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    carol.accept_invite(&invite_id).await;
    carol.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Encrypted at the 3-member epoch.
    alice
        .edit_message(&channel_id, &msg_id, "HELLO edited")
        .await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_msg = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob should still see the original row");
    assert_eq!(
        bob_msg["content"].as_str(),
        Some("HELLO edited"),
        "bob should see alice's edit after a member was added"
    );
    assert!(
        bob_msg["edited_at"].as_str().is_some(),
        "bob's view should carry an edited_at timestamp"
    );

    // Carol has no row for the pre-join original; she must not recover "hello".
    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents: Vec<&str> = carol_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !carol_contents.contains(&"hello"),
        "carol should not see the pre-join plaintext 'hello', got: {carol_contents:?}"
    );

    let carol_view_before_remove: Vec<(String, Option<String>)> = carol
        .fetch_channel_messages(&channel_id)
        .await
        .iter()
        .map(|m| {
            (
                m["id"].as_str().expect("id").to_string(),
                m["content"].as_str().map(str::to_owned),
            )
        })
        .collect();

    alice.remove_member(&group_id, &carol_profile.id).await;
    bob.process_commits_for(&channel_id).await;

    alice
        .edit_message(&channel_id, &msg_id, "HELLO edited again")
        .await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_msg = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob still sees the row");
    assert_eq!(
        bob_msg["content"].as_str(),
        Some("HELLO edited again"),
        "bob should follow the post-remove edit"
    );

    let carol_msgs_after = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents_after: Vec<&str> = carol_msgs_after
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !carol_contents_after.contains(&"HELLO edited again"),
        "removed carol should NOT see the post-remove edit, got: {carol_contents_after:?}"
    );
    // Prior cached value or None, never the new content.
    let prior = carol_view_before_remove
        .iter()
        .find(|(id, _)| id == &msg_id)
        .and_then(|(_, c)| c.clone());
    let now = carol_msgs_after
        .iter()
        .find(|m| m["id"] == msg_id)
        .and_then(|m| m["content"].as_str().map(str::to_owned));
    assert_ne!(
        now.as_deref(),
        Some("HELLO edited again"),
        "carol's row for the edited message must not show the post-remove edit"
    );
    // Removal doesn't wipe what was already decrypted.
    if prior.as_deref() == Some("HELLO edited") {
        assert_eq!(
            now.as_deref(),
            Some("HELLO edited"),
            "carol's cached decrypted copy should not regress after removal"
        );
    }

    let msg_id_v = alice
        .send_channel_message_id(&channel_id, "v1")
        .await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.edit_message(&channel_id, &msg_id_v, "v2").await;

    // Sealed self-edit at an unchanged epoch (#607): 403'd before Solution A.
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_row = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id_v)
        .expect("bob row exists");
    assert_eq!(
        bob_row["content"].as_str(),
        Some("v2"),
        "recipient must apply the author's sealed edit"
    );
    assert!(
        bob_row["edited_at"].as_str().is_some(),
        "the edited row must carry an edited_at timestamp"
    );

    let mut dave = TestClient::new().await;
    let dave_profile = dave.sign_up("dave@test.local").await;
    alice.invite(&group_id, &dave_profile.username).await;
    let invite_id = dave
        .first_pending_invite()
        .await
        .expect("dave invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    dave.accept_invite(&invite_id).await;
    dave.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Must overwrite the first edit; members holding v2 converge on v3.
    alice.edit_message(&channel_id, &msg_id_v, "v3").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_row = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id_v)
        .expect("bob row exists");
    assert_eq!(
        bob_row["content"].as_str(),
        Some("v3"),
        "post-add edit must overwrite the prior edit for a member that had the original"
    );

    // Dave has no row for the pre-join message, so no stale edit can leak.
    let dave_msgs = dave.fetch_channel_messages(&channel_id).await;
    let dave_contents: Vec<&str> = dave_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    for stale in ["v1", "v2"] {
        assert!(
            !dave_contents.contains(&stale),
            "dave must never see stale pre-join plaintext '{stale}', got: {dave_contents:?}"
        );
    }

    drop(alice);
    drop(bob);
    drop(carol);
    drop(dave);
}

/// Raw envelope count, bypassing any command.
async fn envelope_count(remote: &Arc<pollis_delivery::db::Db>, conversation_id: &str) -> i64 {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("count query");
    let row = rows.next().await.expect("row").expect("some row");
    row.get::<i64>(0).expect("count")
}

/// Backdate every envelope in a conversation.
async fn backdate_envelopes(
    remote: &Arc<pollis_delivery::db::Db>,
    conversation_id: &str,
    sent_at: &str,
) {
    let conn = remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE message_envelope SET sent_at = ?1 WHERE conversation_id = ?2",
        libsql::params![sent_at.to_string(), conversation_id.to_string()],
    )
    .await
    .expect("backdate envelopes");
}

/// Wipe a conversation's watermarks so add-member seeding can't mask the gate.
async fn clear_watermarks(
    remote: &Arc<pollis_delivery::db::Db>,
    conversation_id: &str,
) {
    let conn = remote.conn().await.expect("remote conn");
    conn.execute(
        "DELETE FROM conversation_watermark WHERE conversation_id = ?1",
        libsql::params![conversation_id.to_string()],
    )
    .await
    .expect("clear watermarks");
}

/// Drive the DS's own envelope-GC sweep (#689).
async fn run_gc_sweep(remote: &Arc<pollis_delivery::db::Db>) {
    let conn = remote.conn().await.expect("remote conn");
    let stale = pollis_delivery::messages::watermark_stale_modifier();
    pollis_delivery::messages::sweep_envelope_gc(&conn, &stale)
        .await
        .expect("gc sweep");
}

/// The single GC gate is the all-devices delivery watermark; age alone never deletes (F3, I3).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn envelope_cleanup_is_watermark_gated_and_never_ttl_gated() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Cleanup").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    alice.process_commits_for(&channel_id).await;

    // Server-side state, so use the DS's own handle (#987).
    let remote = crate::harness::writable_remote().await;

    // Negative: wipe watermarks so add-member's seeded rows don't satisfy the gate.
    clear_watermarks(&remote, &channel_id).await;

    alice.send_channel_message(&channel_id, "neg-hello").await;
    // Bob has no watermark row, so the gate returns NULL.
    alice.fetch_channel_messages(&channel_id).await;
    run_gc_sweep(&remote).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        1,
        "young envelope with a lagging member should not be cleaned up"
    );

    // Both caught up: the gate is `seq <= MIN(last_seq)` (#1087), so both go.
    bob.fetch_channel_messages(&channel_id).await;
    alice.send_channel_message(&channel_id, "neg-hello-2").await;
    alice.fetch_channel_messages(&channel_id).await;
    bob.fetch_channel_messages(&channel_id).await;
    run_gc_sweep(&remote).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        0,
        "every member device has read past both envelopes, so both are collectable \
         — an envelope AT the floor is handled by everyone (#1087)"
    );

    // F3: rewind every envelope to 2020.
    alice.send_channel_message(&channel_id, "very-old").await;
    backdate_envelopes(&remote, &channel_id, "2020-01-01T00:00:00+00:00").await;
    let ancient = envelope_count(&remote, &channel_id).await;
    assert!(ancient > 0, "fixture must leave ancient envelopes to protect");

    // Bob's watermark is missing, so nothing is deletable however old.
    clear_watermarks(&remote, &channel_id).await;
    alice.fetch_channel_messages(&channel_id).await;
    run_gc_sweep(&remote).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        ancient,
        "ancient envelopes a current member device has NOT collected must ALL \
         survive — retention is bounded by the slowest member device, never by \
         wall-clock age (I3; the old 30-day TTL arm deleted these outright and \
         lost bob's messages)"
    );

    // Still reclaimable once both have fetched.
    bob.fetch_channel_messages(&channel_id).await;
    alice.send_channel_message(&channel_id, "fresh").await;
    alice.fetch_channel_messages(&channel_id).await;
    bob.fetch_channel_messages(&channel_id).await;
    run_gc_sweep(&remote).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        0,
        "once every member device has collected past them, the backdated \
         envelopes are reclaimed — and so is the newest, which they have read too"
    );

    drop(alice);
    drop(bob);
}

/// Distinct commits at one epoch; diagnostic only.
async fn distinct_commits_at_epoch(
    remote: &Arc<pollis_delivery::db::Db>,
    conversation_id: &str,
    epoch: i64,
) -> i64 {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(DISTINCT hex(commit_data)) FROM mls_commit_log \
             WHERE conversation_id = ?1 AND epoch = ?2",
            libsql::params![conversation_id.to_string(), epoch],
        )
        .await
        .expect("distinct commit query");
    let row = rows.next().await.expect("row").expect("some row");
    row.get::<i64>(0).expect("count")
}

/// The Bluestone fork: two admins commit from the same epoch; every current member must still read every other.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn concurrent_commits_at_same_epoch_must_not_fork_a_member() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut dave = TestClient::new().await;
    let mut erin = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;
    let erin_p = erin.sign_up("erin@test.local").await;

    let group_id = alice.create_group("Fork").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Bob must be an admin to invite.
    alice
        .set_member_role(&group_id, bob.user_id(), "admin")
        .await;

    // Branch A: alice commits from the current epoch.
    alice.invite(&group_id, &dave_p.username).await;
    // Branch B: bob, still at the prior epoch, commits from the same one.
    bob.invite(&group_id, &erin_p.username).await;

    // Diagnostic: the fork was produced (on the log DB).
    let log = crate::harness::writable_log().await;
    let distinct = distinct_commits_at_epoch(&log, &group_id, 1).await;
    eprintln!("[test] distinct commits at epoch 1 = {distinct} (fork iff > 1)");

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob.user_id().to_string()),
        "precondition: bob must still be a current group member, got: {members:?}"
    );

    alice.send_channel_message(&channel_id, "from-alice").await;

    // On the forking code bob is stranded on branch B.
    bob.process_commits_for(&channel_id).await;
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        bob_contents.contains(&"from-alice"),
        "FORK: bob (a current member) cannot decrypt alice's message — two \
         commits landed at the same epoch and forked bob onto a divergent \
         tree. got: {bob_msgs:#?}"
    );

    bob.send_channel_message(&channel_id, "from-bob").await;
    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    let alice_contents: Vec<&str> = alice_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        alice_contents.contains(&"from-bob"),
        "FORK: alice cannot decrypt current member bob's message. got: {alice_msgs:#?}"
    );

    drop(alice);
    drop(bob);
    drop(dave);
    drop(erin);
}

/// #411: a lost success response must make alice adopt her landed commit, not wedge.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn lost_submit_response_is_adopted_not_wedged() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Lossy").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    alice.invite(&group_id, &bob_p.username).await;
    let bob_inv = bob
        .first_pending_invite()
        .await
        .expect("bob invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&bob_inv).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // The next commit lands but its response is dropped.
    crate::harness::arm_ds_fault(crate::harness::DsFault::DropResponse);

    alice.invite(&group_id, &carol_p.username).await;

    assert!(
        !crate::harness::ds_fault_armed(),
        "DS lost-response fault should have fired exactly once"
    );

    let carol_inv = carol
        .first_pending_invite()
        .await
        .expect("carol must have a pending invite after the lost-response add")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    carol.accept_invite(&carol_inv).await;
    carol.poll().await;
    alice.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice
        .send_channel_message(&channel_id, "after-lost-response")
        .await;
    bob.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    for (who, client) in [("bob", &bob), ("carol", &carol)] {
        let msgs = client.fetch_channel_messages(&channel_id).await;
        let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            contents.contains(&"after-lost-response"),
            "{who} (a current member) must decrypt alice's message after she adopted her own \
             lost-response commit — committer wedged or member stranded otherwise. got: {msgs:#?}"
        );
    }

    drop(alice);
    drop(bob);
    drop(carol);
}

async fn group_info_row(
    remote: &Arc<pollis_delivery::db::Db>,
    conversation_id: &str,
) -> Option<(i64, usize)> {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT epoch, group_info FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("group_info query");
    match rows.next().await.expect("row") {
        Some(row) => {
            let epoch: i64 = row.get(0).expect("epoch");
            let blob: Vec<u8> = row.get(1).expect("group_info");
            Some((epoch, blob.len()))
        }
        None => None,
    }
}

async fn welcome_count(
    remote: &Arc<pollis_delivery::db::Db>,
    conversation_id: &str,
    recipient_id: &str,
) -> i64 {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_welcome WHERE conversation_id = ?1 AND recipient_id = ?2",
            libsql::params![conversation_id.to_string(), recipient_id.to_string()],
        )
        .await
        .expect("welcome count query");
    let row = rows.next().await.expect("row").expect("some row");
    row.get::<i64>(0).expect("count")
}

/// Commit + Welcome + GroupInfo land atomically through the DS.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn commit_welcome_groupinfo_land_atomically_via_delivery_service() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Atomic").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    // Control-plane tables live on the LOG DB.
    let remote = crate::harness::writable_log().await;

    // The MLS group is keyed by group_id.
    let (epoch0, _) = group_info_row(&remote, &group_id)
        .await
        .expect("epoch-0 GroupInfo should exist after group creation");
    assert_eq!(epoch0, 0, "fresh group's GroupInfo should be at epoch 0");

    alice.invite(&group_id, &bob_p.username).await;

    assert_eq!(
        welcome_count(&remote, &group_id, &bob_p.id).await,
        1,
        "the DS should have written exactly one Welcome for bob alongside the commit"
    );

    // From the commit bundle; the committer no longer republishes.
    let (epoch_after_add, gi_len) = group_info_row(&remote, &group_id)
        .await
        .expect("GroupInfo should exist after the add commit");
    assert_eq!(
        epoch_after_add, 1,
        "the DS-written GroupInfo should sit at the resulting epoch (1)"
    );
    assert!(gi_len > 0, "GroupInfo blob must be non-empty");

    bob.accept_invite(
        bob.first_pending_invite()
            .await
            .expect("bob pending invite")["id"]
            .as_str()
            .expect("invite id"),
    )
    .await;
    bob.poll().await;

    alice.send_channel_message(&channel_id, "atomic-hello").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        bob_contents.contains(&"atomic-hello"),
        "bob should decrypt alice's message after joining via the DS-written Welcome, got: {bob_msgs:#?}"
    );

    drop(alice);
    drop(bob);
}

/// A dropped bootstrap GroupInfo publish is self-healed by the next commit-processing pass.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dropped_bootstrap_group_info_is_healed_on_next_touch() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let _alice_p = alice.sign_up("alice@test.local").await;

    let group_id = alice.create_group("Stranded").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    let log = crate::harness::writable_log().await;

    let (epoch0, _) = group_info_row(&log, &group_id)
        .await
        .expect("epoch-0 GroupInfo should exist after group creation");
    assert_eq!(epoch0, 0, "fresh group's GroupInfo should be at epoch 0");

    // The bricked state a failed create-time publish leaves.
    {
        let conn = log.conn().await.expect("log conn");
        conn.execute(
            "DELETE FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![group_id.clone()],
        )
        .await
        .expect("delete group_info");
    }
    assert!(
        group_info_row(&log, &group_id).await.is_none(),
        "precondition: GroupInfo row should be gone (simulating the dropped publish)"
    );

    // No commit applies here, so only the durability backstop can republish.
    alice.process_commits_for(&channel_id).await;

    let (healed_epoch, len) = group_info_row(&log, &group_id)
        .await
        .expect("GroupInfo should be republished by the self-heal on the next touch");
    assert_eq!(
        healed_epoch, 0,
        "republished GroupInfo should be at the group's current epoch (0)"
    );
    assert!(len > 0, "republished GroupInfo blob should be non-empty");

    drop(alice);
}

/// Invite, accept, and sync every listed client to the new epoch.
async fn add_member_and_sync(
    admin: &TestClient,
    group_id: &str,
    channel_id: &str,
    member: &TestClient,
    profile: &UserProfile,
    others: &[&TestClient],
) {
    admin.invite(group_id, &profile.username).await;
    let invite_id = member
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    member.accept_invite(&invite_id).await;
    member.poll().await;
    admin.process_commits_for(channel_id).await;
    for c in others {
        c.process_commits_for(channel_id).await;
    }
}

/// Delete for everyone redacts copies members already received.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn self_delete_redacts_already_delivered_recipients() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Redaction").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    add_member_and_sync(&alice, &group_id, &channel_id, &bob, &bob_profile, &[]).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &carol, &carol_profile, &[&bob]).await;

    let msg_id = alice
        .send_channel_message_id(&channel_id, "top secret")
        .await;

    for client in [&bob, &carol] {
        let msgs = client.fetch_channel_messages(&channel_id).await;
        let m = msgs
            .iter()
            .find(|m| m["id"] == msg_id)
            .expect("recipient received the message");
        assert_eq!(m["content"].as_str(), Some("top secret"));
        assert!(m["deleted_at"].is_null(), "not deleted yet");
    }

    alice.delete_message(&msg_id).await;

    for (label, client) in [("bob", &bob), ("carol", &carol)] {
        let msgs = client.fetch_channel_messages(&channel_id).await;
        let m = msgs
            .iter()
            .find(|m| m["id"] == msg_id)
            .expect("row is still present as a tombstone");
        assert!(
            m["content"].is_null(),
            "{label}'s copy must be redacted, got content {:?}",
            m["content"]
        );
        assert!(
            m["deleted_at"].as_str().is_some(),
            "{label}'s copy must carry a deleted_at tombstone"
        );
    }

    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    let alice_m = alice_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("alice keeps a tombstone row");
    assert!(alice_m["content"].is_null(), "alice's own copy is redacted");
    assert!(alice_m["deleted_at"].as_str().is_some());

    drop((alice, bob, carol));
}

/// Non-author edits and redactions are rejected: the edit at the DS (#1086), both on ingest for legacy envelopes.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sealed_non_author_edit_is_ignored() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("No Edit Forgery").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &bob, &bob_profile, &[]).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &carol, &carol_profile, &[&bob]).await;

    let msg_id = alice
        .send_channel_message_id(&channel_id, "carol cannot edit this")
        .await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        bob_msgs
            .iter()
            .find(|m| m["id"] == msg_id)
            .expect("bob received it")["content"]
            .as_str(),
        Some("carol cannot edit this")
    );

    // #1086: carol cannot prove the capability, so the write never lands.
    let refused = pollis_core::commands::messages::edit_message_as(
        &carol.state,
        &channel_id,
        &msg_id,
        carol.user_id(),
        "hacked by carol",
    )
    .await;
    assert!(
        refused.is_err(),
        "the DS must refuse an edit from a member who cannot open the target's \
         deletion capability (#1086), got {refused:?}"
    );

    // A real forged redaction; ingest must reject the author mismatch.
    pollis_core::commands::messages::send_redaction_as(
        &carol.state,
        &channel_id,
        &msg_id,
        carol.user_id(),
    )
    .await
    .expect("forged redaction send");

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let m = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("message still present");
    assert_eq!(
        m["content"].as_str(),
        Some("carol cannot edit this"),
        "a non-author edit or redaction must be ignored on ingest"
    );
    assert!(
        m["edited_at"].is_null(),
        "a forged edit must NOT mark the message edited"
    );
    assert!(
        m["deleted_at"].is_null(),
        "a forged redaction must NOT mark the message deleted"
    );

    drop((alice, bob, carol));
}

/// A non-admin author self-deletes; authorship comes from the local row, not the sealed envelope.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sealed_non_admin_self_delete_works() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Self Delete").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &bob, &bob_profile, &[]).await;

    let msg_id = bob
        .send_channel_message_id(&channel_id, "bob's message")
        .await;
    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        alice_msgs
            .iter()
            .find(|m| m["id"] == msg_id)
            .expect("alice received it")["content"]
            .as_str(),
        Some("bob's message")
    );

    bob.delete_message(&msg_id).await;

    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    let m = alice_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("row present as a tombstone");
    assert!(
        m["content"].is_null(),
        "alice's copy must be redacted, got {:?}",
        m["content"]
    );
    assert!(
        m["deleted_at"].as_str().is_some(),
        "alice's copy must carry a deleted_at tombstone"
    );

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE id = ?1 AND type = 'message'",
            libsql::params![msg_id.clone()],
        )
        .await
        .expect("count query");
    let count: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("some row")
        .get(0)
        .expect("count");
    assert_eq!(count, 0, "the self-deleted envelope must be removed from Turso");

    drop((alice, bob));
}

/// #693: classify a failed admin-delete redaction (no tombstone / never fetched / not applied); read-only.
async fn diagnose_admin_delete_failure(
    carol: &TestClient,
    conversation_id: &str,
    msg_id: &str,
) -> String {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");

    let tombstone: Option<String> = {
        let mut rows = conn
            .query(
                "SELECT sent_at FROM message_envelope \
                 WHERE target_message_id = ?1 AND type = 'delete'",
                libsql::params![msg_id.to_string()],
            )
            .await
            .expect("tombstone query");
        rows.next()
            .await
            .expect("row")
            .map(|r| r.get::<String>(0).expect("sent_at"))
    };
    let original_present: bool = {
        let mut rows = conn
            .query(
                "SELECT 1 FROM message_envelope WHERE id = ?1 AND type = 'message'",
                libsql::params![msg_id.to_string()],
            )
            .await
            .expect("original query");
        rows.next().await.expect("row").is_some()
    };

    let carol_device = carol.state.device_id.lock().await.clone().unwrap_or_default();
    let carol_user = carol.user_id();
    let watermark: Option<String> = {
        let mut rows = conn
            .query(
                "SELECT last_fetched_at FROM conversation_watermark \
                 WHERE conversation_id = ?1 AND user_id = ?2 AND device_id = ?3",
                libsql::params![
                    conversation_id.to_string(),
                    carol_user.to_string(),
                    carol_device.clone()
                ],
            )
            .await
            .expect("watermark query");
        rows.next()
            .await
            .expect("row")
            .map(|r| r.get::<String>(0).expect("last_fetched_at"))
    };

    let (local_content, local_deleted_at): (Option<String>, Option<String>) = {
        use rusqlite::OptionalExtension;
        let guard = carol.state.local_db.lock().await;
        let db = guard.as_ref().expect("carol local db");
        db.conn()
            .query_row(
                "SELECT content, deleted_at FROM message WHERE id = ?1",
                rusqlite::params![msg_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .expect("local row query")
            .unwrap_or((None, None))
    };

    let classification = match (&tombstone, &watermark) {
        (None, _) => {
            "TOMBSTONE NEVER WRITTEN — the admin delete did not write a \
             type='delete' row (self-vs-admin misresolution?)."
                .to_string()
        }
        (Some(ts), wm) => {
            let fetched = match wm {
                // No watermark, or one below the tombstone: the fetch would return it.
                None => true,
                Some(w) => ts.as_str() > w.as_str(),
            };
            if fetched {
                "FETCHED-BUT-NOT-APPLIED (candidate 2) — the tombstone sorts \
                 strictly above carol's watermark, so her ingest fetched it, yet \
                 her local row is unredacted. The drop is in ingest.rs's \
                 application of the tombstone, NOT its delivery."
                    .to_string()
            } else {
                format!(
                    "NEVER-FETCHED (candidate 1 — ordering/visibility) — tombstone \
                     sent_at {ts} does NOT sort above carol's watermark {wm:?}, so \
                     her `sent_at > watermark` fetch skipped it. This contradicts \
                     the offline admin_delete_visibility_tests — investigate."
                )
            }
        }
    };

    format!(
        "\n─── #693 admin-delete diagnosis ───\n\
         classification : {classification}\n\
         tombstone.sent_at (server) : {tombstone:?}\n\
         original envelope present  : {original_present}\n\
         carol watermark (server)   : {watermark:?}\n\
         carol local content        : {local_content:?}\n\
         carol local deleted_at      : {local_deleted_at:?}\n\
         carol device_id            : {carol_device}\n\
         ───────────────────────────────────"
    )
}

/// Admin deletes another member's message via the tombstone path.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sealed_admin_delete_of_other_member_works() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Admin Delete").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &bob, &bob_profile, &[]).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &carol, &carol_profile, &[&bob]).await;

    let msg_id = bob
        .send_channel_message_id(&channel_id, "bob's post")
        .await;
    for c in [&alice, &carol] {
        let msgs = c.fetch_channel_messages(&channel_id).await;
        assert_eq!(
            msgs.iter()
                .find(|m| m["id"] == msg_id)
                .expect("received it")["content"]
                .as_str(),
            Some("bob's post")
        );
    }

    alice.delete_message(&msg_id).await;

    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    let m = carol_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("row present as a tombstone");
    // #693: report which mechanism failed; async, so a branch rather than an assert message.
    if !m["content"].is_null() {
        let diag = diagnose_admin_delete_failure(&carol, &channel_id, &msg_id).await;
        panic!(
            "carol's copy must be redacted by the admin tombstone, got {:?}{diag}",
            m["content"]
        );
    }
    assert!(
        m["deleted_at"].as_str().is_some(),
        "carol's copy must carry a deleted_at tombstone"
    );

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT \
               (SELECT COUNT(*) FROM message_envelope WHERE id = ?1 AND type = 'message'), \
               (SELECT COUNT(*) FROM message_envelope WHERE target_message_id = ?1 AND type = 'delete')",
            libsql::params![msg_id.clone()],
        )
        .await
        .expect("count query");
    let row = rows.next().await.expect("row").expect("some row");
    let orig: i64 = row.get(0).expect("orig count");
    let tombstones: i64 = row.get(1).expect("tombstone count");
    assert_eq!(orig, 0, "the admin-deleted original envelope must be removed");
    assert_eq!(tombstones, 1, "an admin delete must write exactly one tombstone");

    drop((alice, bob, carol));
}

/// Admin delete: the author applies a tombstone he did not write to his own row.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn admin_delete_redacts_the_authors_own_copy() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Author Redaction").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member_and_sync(&alice, &group_id, &channel_id, &bob, &bob_profile, &[]).await;

    let msg_id = bob
        .send_channel_message_id(&channel_id, "bob's post")
        .await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        bob_msgs
            .iter()
            .find(|m| m["id"] == msg_id)
            .expect("bob holds his own message")["content"]
            .as_str(),
        Some("bob's post")
    );
    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        alice_msgs
            .iter()
            .find(|m| m["id"] == msg_id)
            .expect("alice received it")["content"]
            .as_str(),
        Some("bob's post")
    );

    alice.delete_message(&msg_id).await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_m = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob's row must remain present as a tombstone, not vanish");
    assert!(
        bob_m["content"].is_null(),
        "the author's own copy must be redacted when an admin deletes it, got {:?}",
        bob_m["content"]
    );
    assert!(
        bob_m["deleted_at"].as_str().is_some(),
        "the author's own copy must carry a deleted_at tombstone"
    );

    drop((alice, bob));
}

/// Threads (#825) behave identically in a DM and a group channel.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn threads_behave_the_same_in_dms_and_channels() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Threads").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    let channel_id = alice.general_channel_id(&group_id).await;

    let channel_root = alice
        .send_channel_message_id(&channel_id, "channel root")
        .await;
    alice
        .send_thread_reply(&channel_id, &channel_root, "channel reply")
        .await;

    let dm_id = alice.create_dm(&[bob_profile.id.as_str()]).await;
    bob.poll().await;
    let dm_root = alice.send_channel_message_id(&dm_id, "dm root").await;
    alice
        .send_thread_reply(&dm_id, &dm_root, "dm reply")
        .await;

    for (label, conversation_id, root_id, reply_text) in [
        ("channel", &channel_id, &channel_root, "channel reply"),
        ("dm", &dm_id, &dm_root, "dm reply"),
    ] {
        let replies = alice.read_thread(root_id).await;
        let texts: Vec<&str> = replies
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(
            texts.contains(&reply_text),
            "{label}: the thread should hold its reply, got: {replies:#?}"
        );

        let feed = alice.fetch_channel_messages(conversation_id).await;
        let feed_has_reply = feed
            .iter()
            .filter_map(|m| m["thread_id"].as_str())
            .any(|t| t == root_id.as_str());
        assert!(
            feed_has_reply,
            "{label}: the reply must still be FETCHABLE with a thread_id set — \
             the channel feed filters it out client-side, so the field is what \
             that filter keys on and it must survive the round trip"
        );

        let summaries = alice.thread_summaries(conversation_id).await;
        let counted = summaries
            .iter()
            .find(|s| s["thread_id"].as_str() == Some(root_id.as_str()))
            .unwrap_or_else(|| {
                panic!("{label}: no thread summary for {root_id}, got: {summaries:#?}")
            });
        assert_eq!(
            counted["reply_count"].as_i64(),
            Some(1),
            "{label}: the root should advertise exactly one reply"
        );
    }

    let _ = alice_profile;
    drop(alice);
    drop(bob);
}

/// Reactions (#931) through the DS: cross-member, grouped, idempotent, per-user removal.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn reactions_round_trip_between_two_members() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Reactions").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    let message_id = alice
        .send_channel_message_id(&channel_id, "worth reacting to")
        .await;

    // Bob must see it: a local-only store would fail this.
    alice
        .invoke_json(
            "add_reaction",
            serde_json::json!({
                "messageId": message_id,
                "userId": alice_profile.id,
                "emoji": "👍",
            }),
        )
        .await;

    let seen_by_bob = bob
        .invoke_json("get_reactions", serde_json::json!({ "messageId": message_id }))
        .await;
    assert_eq!(
        seen_by_bob.as_array().map(|a| a.len()),
        Some(1),
        "bob should see alice's reaction, got: {seen_by_bob:#?}"
    );
    assert_eq!(seen_by_bob[0]["emoji"], "👍");
    assert_eq!(seen_by_bob[0]["count"], 1);

    bob.invoke_json(
        "add_reaction",
        serde_json::json!({
            "messageId": message_id,
            "userId": bob_profile.id,
            "emoji": "👍",
        }),
    )
    .await;

    let grouped = alice
        .invoke_json("get_reactions", serde_json::json!({ "messageId": message_id }))
        .await;
    assert_eq!(
        grouped.as_array().map(|a| a.len()),
        Some(1),
        "the same emoji from two users must group into one entry, got: {grouped:#?}"
    );
    assert_eq!(grouped[0]["count"], 2, "count should accumulate: {grouped:#?}");

    // The UI toggle relies on a repeat add being a no-op.
    alice
        .invoke_json(
            "add_reaction",
            serde_json::json!({
                "messageId": message_id,
                "userId": alice_profile.id,
                "emoji": "👍",
            }),
        )
        .await;
    let after_repeat = alice
        .invoke_json("get_reactions", serde_json::json!({ "messageId": message_id }))
        .await;
    assert_eq!(
        after_repeat[0]["count"], 2,
        "re-adding an existing reaction must not duplicate it: {after_repeat:#?}"
    );

    // Must leave Bob's identical reaction alone.
    alice
        .invoke_json(
            "remove_reaction",
            serde_json::json!({
                "messageId": message_id,
                "userId": alice_profile.id,
                "emoji": "👍",
            }),
        )
        .await;

    let after_remove = bob
        .invoke_json("get_reactions", serde_json::json!({ "messageId": message_id }))
        .await;
    assert_eq!(
        after_remove.as_array().map(|a| a.len()),
        Some(1),
        "bob's reaction must survive alice's removal, got: {after_remove:#?}"
    );
    assert_eq!(
        after_remove[0]["count"], 1,
        "only alice's reaction should be gone: {after_remove:#?}"
    );
    let remaining: Vec<&str> = after_remove[0]["user_ids"]
        .as_array()
        .expect("user_ids")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        remaining,
        vec![bob_profile.id.as_str()],
        "the surviving reaction must be bob's"
    );

    drop(alice);
    drop(bob);
}

async fn react(client: &TestClient, user_id: &str, message_id: &str, emoji: &str) {
    client
        .invoke_json(
            "add_reaction",
            serde_json::json!({
                "messageId": message_id,
                "userId": user_id,
                "emoji": emoji,
            }),
        )
        .await;
}

/// #1433: reactions survive GC of the message's envelope (#689).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn reactions_survive_envelope_collection() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Collected").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    alice.process_commits_for(&channel_id).await;
    let early = alice
        .send_channel_message_id(&channel_id, "reacted to before the sweep")
        .await;
    let late = alice
        .send_channel_message_id(&channel_id, "reacted to after the sweep")
        .await;
    bob.fetch_channel_messages(&channel_id).await;
    react(&bob, &bob_profile.id, &early, "👀").await;

    alice.fetch_channel_messages(&channel_id).await;
    bob.fetch_channel_messages(&channel_id).await;
    let remote = crate::harness::writable_remote().await;
    run_gc_sweep(&remote).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        0,
        "fixture: both envelopes must have been collected"
    );

    react(&alice, &alice_profile.id, &late, "👍").await;
    for (who, client) in [("alice", &alice), ("bob", &bob)] {
        let seen = client
            .invoke_json("get_reactions", serde_json::json!({ "messageId": late }))
            .await;
        assert_eq!(
            seen.as_array().map(|a| a.len()),
            Some(1),
            "{who} must see a reaction added after the envelope was collected: {seen:#?}"
        );
        assert_eq!(seen[0]["emoji"], "👍");
    }

    let seen = alice
        .invoke_json("get_reactions", serde_json::json!({ "messageId": early }))
        .await;
    assert_eq!(
        seen.as_array().map(|a| a.len()),
        Some(1),
        "a reaction made while the envelope existed must outlive it: {seen:#?}"
    );
    assert_eq!(seen[0]["emoji"], "👀");

    drop(alice);
    drop(bob);
}

/// #1087: DS-assigned sequences increase and the recipient's cursor advances past them.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_real_send_assigns_a_delivery_sequence_and_advances_the_cursor() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("SeqCursor").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob has an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    let first = alice.send_channel_message_id(&channel_id, "one").await;
    let second = alice.send_channel_message_id(&channel_id, "two").await;

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");

    let seq_of = |id: String| {
        let conn = conn.clone();
        async move {
            let mut rows = conn
                .query(
                    "SELECT seq FROM message_envelope WHERE id = ?1",
                    libsql::params![id],
                )
                .await
                .expect("seq query");
            rows.next()
                .await
                .expect("row")
                .expect("the envelope must exist")
                .get::<Option<i64>>(0)
                .expect("seq column")
        }
    };

    let s1 = seq_of(first.clone())
        .await
        .expect("a real send must land with a delivery sequence");
    let s2 = seq_of(second.clone())
        .await
        .expect("a real send must land with a delivery sequence");
    assert!(s1 < s2, "sequences must increase: {s1} then {s2}");

    bob.fetch_channel_messages(&channel_id).await;

    let cursor: Option<i64> = {
        let mut rows = conn
            .query(
                "SELECT last_seq FROM conversation_watermark \
                 WHERE conversation_id = ?1 AND user_id = ?2",
                libsql::params![channel_id.clone(), bob_profile.id.clone()],
            )
            .await
            .expect("watermark query");
        rows.next()
            .await
            .expect("row")
            .expect("bob's device has a watermark row")
            .get::<Option<i64>>(0)
            .expect("last_seq column")
    };
    assert_eq!(
        cursor,
        Some(s2),
        "bob handled both messages, so his cursor must sit AT the highest — \
         the cursor is inclusive, and a cursor that lagged would re-deliver \
         while one that ran ahead would skip"
    );

    drop(alice);
    drop(bob);
}

/// The live path: ingest, then re-read the LOCAL page.
async fn live_receive(client: &TestClient, conversation_id: &str, is_channel: bool) -> Vec<String> {
    let (ingest, read, key) = if is_channel {
        ("ingest_channel_envelopes", "read_channel_messages", "channelId")
    } else {
        ("ingest_dm_envelopes", "read_dm_messages", "dmChannelId")
    };
    let mut ingest_args = json!({ "userId": client.user_id() });
    ingest_args[key] = json!(conversation_id);
    client.invoke_json(ingest, ingest_args).await;
    let mut read_args = json!({ "limit": 50 });
    read_args[key] = json!(conversation_id);
    let page = client.invoke_json(read, read_args).await;
    page["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_owned))
        .collect()
}

/// Own messages reach the user's other device live; dedupe keys on message id only.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_users_own_message_reaches_their_other_device_live() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice_profile = alice_d1.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let early_group = alice_d1.create_group("Early").await;
    let early_channel = alice_d1.general_channel_id(&early_group).await;
    add_member_and_sync(&alice_d1, &early_group, &early_channel, &bob, &bob_profile, &[]).await;

    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;

    let late_group = alice_d1.create_group("Late").await;
    let late_channel = alice_d1.general_channel_id(&late_group).await;
    add_member_and_sync(&alice_d1, &late_group, &late_channel, &bob, &bob_profile, &[&alice_d2])
        .await;
    let dm_id = alice_d1
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;
    bob.accept_dm_request(&dm_id).await;

    for c in [&alice_d1, &alice_d2, &bob] {
        c.poll().await;
        c.sweep().await;
    }

    for (label, conversation_id, is_channel) in [
        ("early channel", &early_channel, true),
        ("late channel", &late_channel, true),
        ("dm", &dm_id, false),
    ] {
        let from_d1 = format!("{label}: from alice_d1");
        alice_d1.send_channel_message(conversation_id, &from_d1).await;
        let on_d2 = live_receive(&alice_d2, conversation_id, is_channel).await;
        assert!(
            on_d2.contains(&from_d1),
            "{label}: alice_d2 must receive alice_d1's message live, got {on_d2:?}"
        );
        let on_bob = live_receive(&bob, conversation_id, is_channel).await;
        assert!(on_bob.contains(&from_d1), "{label}: bob must receive it too, got {on_bob:?}");

        let from_d2 = format!("{label}: from alice_d2");
        alice_d2.send_channel_message(conversation_id, &from_d2).await;
        let on_d1 = live_receive(&alice_d1, conversation_id, is_channel).await;
        assert!(
            on_d1.contains(&from_d2),
            "{label}: alice_d1 must receive alice_d2's message live, got {on_d1:?}"
        );

        let from_bob = format!("{label}: from bob");
        bob.send_channel_message(conversation_id, &from_bob).await;
        for (client, who) in [(&alice_d1, "alice_d1"), (&alice_d2, "alice_d2")] {
            let got = live_receive(client, conversation_id, is_channel).await;
            assert!(got.contains(&from_bob), "{label}: {who} must receive bob's message, got {got:?}");
        }
    }

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}
