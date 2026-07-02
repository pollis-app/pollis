use std::sync::Arc;

use crate::harness::{wipe, TestClient};
use pollis_lib::commands::auth::UserProfile;
use serial_test::serial;

/// End-to-end crypto round-trip: after Bob accepts Alice's invite, Alice
/// sends a channel message. Bob fetches the channel via `get_channel_messages`
/// and sees the decrypted plaintext — proving the MLS handshake + encrypt +
/// decrypt path works through the real command pipeline.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn channel_message_round_trip() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Crypto").await;
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

    alice.send_channel_message(&channel_id, "hello bob").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let contents: Vec<&str> = bob_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        contents.contains(&"hello bob"),
        "bob should decrypt alice's message, got: {bob_msgs:#?}"
    );

    drop(alice);
    drop(bob);
}

/// Scales the group up from 1 → 9 members and back down to 1, sending a
/// labeled message after every membership change. Verifies:
///   - After each add, every current member (including the newcomer) can
///     decrypt the subsequently-sent message. Newcomers cannot decrypt
///     messages that were sent before they joined — MLS Welcomes do not
///     carry history, so `content` comes back as null for pre-join envelopes.
///   - After each remove, the removed member can no longer decrypt any
///     subsequent message (their epoch is stale; their keys don't open the
///     new commit).
///   - Every surviving member's epoch ratchet stays consistent: they keep
///     decrypting every post-change message, proving that commit processing
///     (add and remove) advances their local MLS state correctly.
///   - The creator can finish the test alone and still send + decrypt —
///     the group state survives the full shrink.
///
/// This is the main stress test for MLS epoch ratcheting under heavy
/// membership churn.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn epoch_ratchet_nine_users_add_remove() {
    wipe().await;

    // Nine fully-signed-up clients. Index 0 is the creator and stays for
    // the whole test; indices 1..9 are added in order and then removed in
    // reverse order.
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

    // ── Growth phase: 2 → 9 members ──
    for n in 1..9 {
        // Creator invites user n+1.
        let invitee_username = profiles[n].username.clone();
        clients[0].invite(&group_id, &invitee_username).await;

        // Invitee accepts + applies their Welcome.
        let invite = clients[n]
            .first_pending_invite()
            .await
            .unwrap_or_else(|| panic!("user{} should have pending invite", n + 1));
        let invite_id = invite["id"].as_str().expect("invite id").to_string();
        clients[n].accept_invite(&invite_id).await;
        clients[n].poll().await;

        // Already-members apply the add commit so their local epoch advances.
        for k in 0..n {
            clients[k].process_commits_for(&channel_id).await;
        }

        // Creator sends a step-labeled message.
        let label = format!("msg-{n}");
        clients[0].send_channel_message(&channel_id, &label).await;

        // Every current member decrypts the latest message.
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

        // Newcomer cannot decrypt any of the prior messages — they joined
        // after those were encrypted to older epochs.
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

    // ── Shrink phase: 9 → 1 members ──
    // Remove users 9, 8, ..., 2 (user 0 is creator and stays). After each
    // removal, send a new message and assert (a) every remaining member
    // decrypts it, (b) the just-removed user cannot.
    for n in (1..9).rev() {
        let target_id = profiles[n].id.clone();
        clients[0].remove_member(&group_id, &target_id).await;

        // Remaining members apply the remove commit.
        for k in 0..n {
            clients[k].process_commits_for(&channel_id).await;
        }

        let label = format!("post-remove-{}", n + 1);
        clients[0].send_channel_message(&channel_id, &label).await;

        // Remaining members decrypt.
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

        // Removed user cannot decrypt the post-removal message.
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

    // Creator alone at epoch N can still send + decrypt.
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

/// Seeded xorshift64 — deterministic and dependency-free. We intentionally
/// avoid pulling `StdRng` in from `rand` here so changes to its default
/// feature set can't silently re-seed this scenario.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // 0 is a fixed point for xorshift; nudge it if the caller passes 0.
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

    /// Uniform `[0, n)`.
    fn gen_range(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next_u64() as usize) % n
    }
}

/// Seeded random churn across membership and role changes. At every step the
/// harness verifies that:
///   - `get_group_members` on the creator exactly matches the scenario's
///     expected membership and per-member admin flag.
///   - A message sent by the current admin or a current non-admin member
///     is decryptable by every present member and opaque to every
///     non-present client (including members removed earlier in the run).
///
/// This catches subtle regressions the linear growth test can't hit:
///   - An admin's epoch becoming stale across role toggles.
///   - An add/remove/add sequence failing to advance a long-present member's
///     ratchet.
///   - `set_member_role` silently mutating membership state.
///
/// Uses a fixed xorshift seed; failure logs print the full op sequence via
/// `eprintln!` so a red run is reproducible.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn random_admin_and_membership_churn() {
    wipe().await;

    const NUM_OTHERS: usize = 5;
    const MAX_GROUP_SIZE: usize = 5;
    const NUM_OPS: usize = 30;
    const SEED: u64 = 0xC0FFEE_BEEF_F00D;

    // Index 0 is the creator; 1..=5 are other pre-signed-up clients.
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

    // Tracked expected state: who is a member, who is an admin. The creator
    // is always both and stays in the group.
    use std::collections::BTreeSet;
    let mut members: BTreeSet<usize> = BTreeSet::from([0]);
    let mut admins: BTreeSet<usize> = BTreeSet::from([0]);

    // Ordered log of ops executed, printed on failure for reproducibility.
    let mut op_log: Vec<String> = Vec::new();
    let mut msg_counter: u32 = 0;

    let mut rng = XorShift64::new(SEED);

    for op_idx in 0..NUM_OPS {
        // Pick a candidate op; skip if it can't apply.
        let pick = rng.gen_range(6);
        let op_name: &'static str = match pick {
            0 => "add",
            1 => "remove",
            2 => "promote",
            3 => "demote",
            4 => "send_from_admin",
            _ => "send_from_member",
        };

        // Precompute the population each op needs so we can skip cleanly.
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

                // Every already-member (including creator) applies the add commit.
                for &k in members.iter() {
                    clients[k].process_commits_for(&channel_id).await;
                }
                // Newcomer also processes commits — `poll` applied the Welcome but
                // later commits could have landed before this point in a real run.
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

        // After every op, verify membership + role state matches expectations.
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

/// Shared assertion for send-from-admin / send-from-member ops in the churn
/// scenario: every current member must decrypt the message; no non-member
/// may see its plaintext.
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

/// Exercises `edit_message` across MLS membership changes. Three phases in
/// one scenario:
///
/// 1. **Add-then-edit**: after alice+bob are in the group, alice sends
///    "hello"; bob fetches and sees it. Carol is invited and joins; alice
///    then edits the message. Bob and carol both see the edited content on
///    their next fetch. Carol never saw "hello" (Welcomes don't carry
///    history) — that's fine; we only assert she can decrypt the edit once
///    she's a member because `edit_message` re-encrypts at the post-add
///    epoch.
/// 2. **Remove-then-edit**: alice removes carol, then edits again. Bob
///    picks up the new edit; carol does not — the edit envelope is
///    encrypted to an epoch she's no longer in, so her local cache is
///    frozen at whatever she last decrypted.
/// 3. **Edit → add → edit**: final sanity check that edits written across
///    a second add advance too. Alice edits ("v2"), adds a new user, then
///    edits again ("v3"). All current members converge on "v3", proving
///    the second edit's ciphertext targets the correct (post-add) epoch
///    and that a stale first edit cannot linger and win — `edit_message`
///    does a DELETE+INSERT so only the latest envelope survives.
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

    // ── Phase 1: add-then-edit ──
    // alice + bob are in the group; alice sends "hello".
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

    // Bob can decrypt the original pre-add.
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"hello"),
        "bob should decrypt 'hello' before carol is added, got: {bob_contents:?}"
    );

    // Now add carol and advance everyone's epoch.
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

    // Alice edits; the edit envelope is encrypted at the 3-member epoch.
    alice
        .edit_message(&channel_id, &msg_id, "HELLO edited")
        .await;

    // Bob's local cache flips to the edited content.
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

    // Carol fetches too. She never had a local row for `msg_id` (the
    // original envelope was encrypted at the pre-join epoch she can't
    // decrypt), and the current edit-apply path only UPDATEs an existing
    // row — so carol's view of this specific message is null content.
    // The important property here is that she must NOT have recovered the
    // pre-join plaintext "hello".
    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents: Vec<&str> = carol_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !carol_contents.contains(&"hello"),
        "carol should not see the pre-join plaintext 'hello', got: {carol_contents:?}"
    );

    // ── Phase 2: remove-then-edit ──
    // Snapshot carol's view BEFORE removal so we can confirm her cache
    // freezes after she loses access.
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

    // Alice publishes a second edit at the post-remove epoch.
    alice
        .edit_message(&channel_id, &msg_id, "HELLO edited again")
        .await;

    // Bob follows the edit.
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

    // Carol cannot decrypt the new envelope; her local cache is frozen at
    // whatever she last saw. She definitely must not see the new content.
    let carol_msgs_after = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents_after: Vec<&str> = carol_msgs_after
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !carol_contents_after.contains(&"HELLO edited again"),
        "removed carol should NOT see the post-remove edit, got: {carol_contents_after:?}"
    );
    // Her view of msg_id either retains the prior cached value or is None,
    // but never the new content.
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
    // If carol had cached the first edit, she should still have it — removal
    // shouldn't retroactively wipe what's already decrypted locally.
    if prior.as_deref() == Some("HELLO edited") {
        assert_eq!(
            now.as_deref(),
            Some("HELLO edited"),
            "carol's cached plaintext should not regress after removal"
        );
    }

    // ── Phase 3: edit → add → edit ──
    // Fresh message on the current (post-remove) epoch.
    let msg_id_v = alice
        .send_channel_message_id(&channel_id, "v1")
        .await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.edit_message(&channel_id, &msg_id_v, "v2").await;

    // Add a brand-new user dave; advance everyone's epoch.
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

    // Second edit at the post-add epoch — must overwrite the first edit,
    // which is still sitting in `message_envelope`. DELETE+INSERT inside
    // edit_message guarantees only "v3" survives remotely, so a member
    // whose local cache holds v2 must converge on v3 (never see "v2" or
    // the original "v1" after fetching).
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

    // Dave — like carol earlier — has no local row for the pre-join
    // message, so his fetch returns content=None and the prior edits
    // can't "leak through" as cached plaintext. The invariant we do
    // care about is that he never observes a stale prior edit as
    // plaintext.
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

/// Count remaining envelopes for a conversation via a direct libsql query.
/// Bypasses any Tauri command so we observe the raw row state.
async fn envelope_count(remote: &Arc<pollis_lib::db::remote::RemoteDb>, conversation_id: &str) -> i64 {
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

/// Backdate every envelope in a conversation to a known timestamp. Used to
/// simulate old envelopes without waiting 30 days. Applies to envelopes of
/// any type.
async fn backdate_envelopes(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
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

/// Wipe conversation watermarks for a specific conversation so the test can
/// reconstruct a known lag pattern. Add-member seeding would otherwise leave
/// recent watermarks that mask the behavior we want to exercise.
async fn clear_watermarks(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
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

/// Exercises the two independent gates in `get_channel_messages`' envelope
/// cleanup: the 30-day TTL and the all-members-caught-up watermark. They're
/// OR'd, so either alone is sufficient to delete. The scenario drives
/// three cases, each time triggering cleanup by having a member fetch:
///
/// - **Negative**: young envelope, only the sender has fetched. Neither
///   gate fires — envelope stays.
/// - **Watermark-only**: young envelope, both members have fetched past it.
///   Watermark gate fires even though TTL is far from expired.
/// - **TTL-only**: envelope backdated past 30 days while watermarks are
///   deliberately left in a state where the watermark gate cannot fire
///   (one member's row is absent, so the CASE returns NULL). TTL gate
///   fires alone and the envelope is deleted.
///
/// The backdating + watermark hacks poke the remote DB directly — there's
/// no production command that lets a test manipulate `sent_at` or
/// `conversation_watermark`, and we intentionally don't add one.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn envelope_cleanup_ttl_or_watermark() {
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

    // The backdating + watermark hacks below poke the "server" DB directly,
    // standing in for server-side envelope GC effects. The client's own
    // `state.remote_db` is a read-only view, so use the writable world handle.
    let remote = crate::harness::writable_remote().await;

    // ── Negative: young envelope, only sender fetched ──
    // Wipe watermarks first so add-member's seeded "now" rows don't satisfy
    // the watermark gate on their own.
    clear_watermarks(&remote, &channel_id).await;

    alice.send_channel_message(&channel_id, "neg-hello").await;
    // Alice fetches — triggers cleanup. Bob has not fetched. The cleanup
    // subquery requires every current member to have a watermark row; bob
    // does not, so the watermark gate returns NULL. TTL is fresh (< 30
    // days). Neither gate fires — envelope stays.
    alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        1,
        "young envelope with a lagging member should not be cleaned up"
    );

    // ── Watermark-only: young envelope, both watermarks strictly past it ──
    // The cleanup query uses `sent_at < MIN(cw)`, and the watermark upsert
    // uses the latest returned message's `sent_at`. So to delete "neg-hello"
    // via the watermark gate we need a STRICTLY later message that both
    // members have fetched — that later message's sent_at becomes the new
    // watermark, and neg-hello's sent_at becomes strictly less than it.
    bob.fetch_channel_messages(&channel_id).await;
    alice.send_channel_message(&channel_id, "neg-hello-2").await;
    alice.fetch_channel_messages(&channel_id).await;
    bob.fetch_channel_messages(&channel_id).await;
    // Both watermarks now sit at sent_at("neg-hello-2"), strictly greater
    // than sent_at("neg-hello"). The next cleanup trigger evicts the older
    // envelope but leaves the newer one (whose sent_at equals MIN).
    alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        1,
        "older envelope should be cleaned once every watermark passes it, while the latest envelope remains"
    );

    // ── TTL-only: old envelope, watermark gate deliberately broken ──
    // Send a fresh envelope, then backdate it past the 30-day TTL.
    alice.send_channel_message(&channel_id, "very-old").await;
    // Rewind sent_at into the past — well beyond the 30-day threshold.
    backdate_envelopes(&remote, &channel_id, "2020-01-01T00:00:00+00:00").await;
    // Wipe watermarks again so the gate cannot accidentally fire: alice's
    // upsert during her fetch will re-create her row (set to the backdated
    // sent_at), but bob will be missing until he fetches.  `COUNT(gm) !=
    // COUNT(cw)` → CASE returns NULL → watermark gate stays false.
    clear_watermarks(&remote, &channel_id).await;
    alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        0,
        "old envelope should be cleaned by the TTL gate even when the watermark gate cannot fire"
    );

    drop(alice);
    drop(bob);
}

/// Count distinct commit blobs at a given epoch for a conversation. Used to
/// confirm the fork precondition (two competing commits at one epoch) was
/// actually produced — diagnostic only, not an invariant.
async fn distinct_commits_at_epoch(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
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

/// Reproduces the Bluestone production fork (prod group `01KQYX89...`).
///
/// Two admins commit from the SAME MLS epoch before either has processed the
/// other's commit. Both commits are posted to `mls_commit_log` at the same
/// epoch — and because the log has no uniqueness on `(conversation_id, epoch)`,
/// BOTH land. Every member then processes `ORDER BY epoch ASC, seq ASC` and
/// applies the lower-`seq` commit (branch A), advancing past it. The author of
/// the higher-`seq` commit already merged its OWN commit (branch B) locally,
/// so it sits on a divergent epoch-N tree and can never apply branch A's later
/// commits — it is permanently forked.
///
/// Symptom (matches the prod report): the forked member and the branch-A
/// members cannot decrypt each other's messages, even though group membership
/// never changed.
///
/// This asserts the product invariant — every CURRENT member can message every
/// other current member — so it FAILS on today's forking code and should PASS
/// once two commits racing for the same epoch are resolved by a
/// compare-and-swap (only one wins; the loser rolls back its local merge and
/// re-applies the winner before retrying).
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

    // alice creates the group and adds bob; both settle at the same epoch
    // with the same tree {alice, bob}.
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

    // bob must be an admin to invite (mirrors Bluestone: all members admin).
    alice
        .set_member_role(&group_id, bob.user_id(), "admin")
        .await;

    // ── Concurrent commits from the same epoch ──
    // alice invites dave: alice reconciles and commits (add dave) from the
    // current epoch → branch A. This merges locally and gets the lower seq.
    alice.invite(&group_id, &dave_p.username).await;
    // bob has NOT processed alice's commit, so bob is still at the prior
    // epoch. bob invites erin: bob reconciles and commits from that SAME
    // epoch → branch B, higher seq. Two distinct commits now occupy one epoch.
    bob.invite(&group_id, &erin_p.username).await;

    // Precondition sanity (diagnostic): the fork was actually produced. The
    // add-bob commit was epoch 0, so the concurrent adds race for epoch 1.
    // The commit log lives on the LOG DB (split harness), so inspect it there.
    let log = alice.state.log_db.clone();
    let distinct = distinct_commits_at_epoch(&log, &group_id, 1).await;
    eprintln!("[test] distinct commits at epoch 1 = {distinct} (fork iff > 1)");

    // Membership never shrank — bob is still a current member.
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob.user_id().to_string()),
        "precondition: bob must still be a current group member, got: {members:?}"
    );

    // alice sends from branch A.
    alice.send_channel_message(&channel_id, "from-alice").await;

    // INVARIANT: bob, a current member, must be able to read alice's message.
    // On the forking code bob is stranded on branch B and cannot — its content
    // comes back null.
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

    // And the reverse: bob sends, alice (branch A) must read it.
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

/// Issue #411: a lost SUCCESS-response must not wedge the committer or strand
/// the member they just added. The DS writes the add-carol commit (it lands
/// canonically) but alice sees a network error. The fix must make alice ADOPT
/// her own landed commit — merge to advance her epoch AND write carol's Welcome
/// + publish GroupInfo — rather than roll back, delete her group, and wedge.
///
/// On the pre-fix code alice clears the pending commit and never writes the
/// Welcome, so carol can't join and alice later self-deletes against a stale
/// GroupInfo: both assertions below fail.
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

    // Add bob and settle both at a shared epoch.
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

    // Arm the one-shot fault on the in-process Delivery Service: alice's next
    // commit lands (commit + Welcome + GroupInfo all written) but the DS's
    // success response is dropped — a lost success-response on the real HTTP
    // path. The client must adopt its own canonical commit, not wedge.
    crate::harness::arm_ds_fault(crate::harness::DsFault::DropResponse);

    alice.invite(&group_id, &carol_p.username).await;

    assert!(
        !crate::harness::ds_fault_armed(),
        "DS lost-response fault should have fired exactly once"
    );

    // INVARIANT 1 — carol's Welcome was written despite the lost response, so
    // she can join.
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

    // INVARIANT 2 — alice did not wedge: she can still send, and both the
    // existing member (bob) and the member added through the lost-response
    // commit (carol) decrypt it.
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

/// #356 — a device whose `user_device` row has been deleted (the revoked-device
/// state) must not be able to climb back into a group it was removed from.
///
/// Modeled cross-user: each `TestClient` user gets its own local DB, whereas
/// the harness shares one local DB per `user_id` and so cannot represent two
/// independent devices of the *same* user (that intra-user path is validated by
/// manual multi-device testing). The MLS mechanics are identical either way —
/// a leaf whose device cert is gone must fail cross-signing verification, so the
/// device cannot rejoin. Before this fix the verification was advisory, so a
/// removed device with live Turso write creds external-joined straight back in.
///
/// Asserts: after removal + cert deletion the device does NOT auto-rejoin,
/// cannot read post-removal messages, and does not wedge the group (the
/// rejected self-add must not be allowed to squat the epoch under the
/// UNIQUE(conversation_id, epoch) constraint).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn revoked_device_cannot_rejoin_group() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Revoke").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // bob + carol join.
    for (c, p) in [(&bob, &bob_p), (&carol, &carol_p)] {
        alice.invite(&group_id, &p.username).await;
        let invite_id = c
            .first_pending_invite()
            .await
            .expect("invite")["id"]
            .as_str()
            .expect("invite id")
            .to_string();
        c.accept_invite(&invite_id).await;
        c.poll().await;
        alice.process_commits_for(&channel_id).await;
    }
    bob.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    // Baseline: everyone decrypts.
    alice.send_channel_message(&channel_id, "before").await;
    for (c, label) in [(&alice, "alice"), (&bob, "bob"), (&carol, "carol")] {
        let msgs = c.fetch_channel_messages(&channel_id).await;
        let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            contents.contains(&"before"),
            "{label} should decrypt 'before', got: {contents:?}"
        );
    }

    // Revoke bob's device: tombstone its `user_device` row (issue #372) so
    // verify_added_devices distinguishes "revoked" (delete the squatting
    // commit OK) from "absent because not replicated yet" (don't delete).
    // Pre-#372 this was a hard DELETE; the migration to tombstones changed
    // revoke_device to set revoked_at, and this test mirrors that path.
    {
        // Server-side revocation effect — poke the writable "server" handle
        // directly (the client's `state.remote_db` is a read-only view).
        let remote = crate::harness::writable_remote().await;
        let conn = remote.conn().await.expect("remote conn");
        conn.execute(
            "UPDATE user_device SET revoked_at = datetime('now') WHERE user_id = ?1",
            libsql::params![bob_p.id.clone()],
        )
        .await
        .expect("tombstone bob user_device");
    }

    // Remove bob from the group — reconcile prunes his leaf.
    alice.remove_member(&group_id, &bob_p.id).await;
    alice.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    // bob syncs: evicted → must NOT external-join back in (device row gone).
    bob.fetch_channel_messages(&channel_id).await;

    // alice sends after the revoke.
    alice.send_channel_message(&channel_id, "after-revoke").await;

    // alice + carol read it; bob (revoked, out of the tree) does not.
    for (c, label) in [(&alice, "alice"), (&carol, "carol")] {
        let msgs = c.fetch_channel_messages(&channel_id).await;
        let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            contents.contains(&"after-revoke"),
            "{label} should decrypt 'after-revoke', got: {contents:?}"
        );
    }
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        !bob_contents.contains(&"after-revoke"),
        "REVOCATION BYPASS: revoked bob decrypted a post-revoke message — it rejoined the group. got: {bob_contents:?}"
    );

    // No wedge: the group keeps advancing after the rejected rejoin attempt.
    alice.send_channel_message(&channel_id, "after-2").await;
    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents: Vec<&str> = carol_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        carol_contents.contains(&"after-2"),
        "group wedged after revoke — carol could not receive a new message. got: {carol_contents:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// Read the `mls_group_info` row (epoch, byte length) for a conversation, if any.
async fn group_info_row(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
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

/// Count `mls_welcome` rows for a recipient in a conversation.
async fn welcome_count(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
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

/// Slice 1 — commit + Welcome + GroupInfo land atomically through the Delivery
/// Service. The whole flows harness routes submission through an in-process
/// `pollis-delivery` instance (see `harness::world`), so this asserts the DS
/// path specifically:
///
///   1. When alice adds bob, the DS writes the add commit, bob's Welcome, AND
///      the resulting-epoch GroupInfo as one unit — all three rows are present
///      remotely after the single `invite` call (no separate inline Welcome
///      write, no post-merge GroupInfo republish).
///   2. Bob joins purely from the DS-written Welcome and decrypts alice's
///      message — proving the Welcome the DS persisted is the real one.
///   3. The GroupInfo the DS wrote sits at the resulting epoch (epoch after the
///      add), so a future device could external-join from it.
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
    // The MLS control-plane tables (`mls_group_info`, `mls_welcome`) live on the
    // LOG DB in the split harness — inspect them there, not on the main DB.
    let remote = alice.state.log_db.clone();

    // Baseline GroupInfo: init_mls_group published the epoch-0 GroupInfo. The
    // MLS group is keyed by group_id (the channel's group), so that's the
    // conversation id for the control-plane rows.
    let (epoch0, _) = group_info_row(&remote, &group_id)
        .await
        .expect("epoch-0 GroupInfo should exist after group creation");
    assert_eq!(epoch0, 0, "fresh group's GroupInfo should be at epoch 0");

    // Alice invites bob. reconcile builds the add commit and hands commit +
    // Welcome + GroupInfo to the delivery seam, which here is the in-process
    // Delivery Service over HTTP. The DS writes all three atomically on the win.
    alice.invite(&group_id, &bob_p.username).await;

    // (1) The DS wrote bob's Welcome.
    assert_eq!(
        welcome_count(&remote, &group_id, &bob_p.id).await,
        1,
        "the DS should have written exactly one Welcome for bob alongside the commit"
    );

    // (1) The DS advanced the stored GroupInfo to the resulting epoch (1). The
    // committer no longer republishes GroupInfo after the merge — this row came
    // from the commit bundle, written atomically with the commit by the DS.
    let (epoch_after_add, gi_len) = group_info_row(&remote, &group_id)
        .await
        .expect("GroupInfo should exist after the add commit");
    assert_eq!(
        epoch_after_add, 1,
        "the DS-written GroupInfo should sit at the resulting epoch (1)"
    );
    assert!(gi_len > 0, "GroupInfo blob must be non-empty");

    // (2) Bob joins purely from the DS-written Welcome, then decrypts a message.
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

/// Regression: a dropped bootstrap GroupInfo publish is self-healed on the next
/// "group touched" pass, instead of bricking the group forever.
///
/// The bug: `init_mls_group` publishes the epoch-0 GroupInfo best-effort. If that
/// DS post fails (a transient outage right as the group is created), nothing ever
/// retried it — the post-merge republish only fired when a commit was applied
/// (`any_applied`), which is never true for a freshly created, sole-member group.
/// With no GroupInfo in the log DB, no member could ever external-join, so a group
/// created during an outage was permanently unjoinable (and any message sent into
/// it was silently lost).
///
/// We reproduce the stranded state directly — delete the epoch-0 GroupInfo row
/// from the log DB (exactly what a failed publish leaves behind) — then run an
/// ordinary commit-processing pass and assert the row reappears at the same epoch.
/// Before the fix this pass was a no-op and the row stayed gone.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dropped_bootstrap_group_info_is_healed_on_next_touch() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let _alice_p = alice.sign_up("alice@test.local").await;

    let group_id = alice.create_group("Stranded").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    // MLS control-plane rows live on the LOG DB; the MLS group is keyed by group_id.
    let log = alice.state.log_db.clone();

    // Baseline: init_mls_group published the epoch-0 GroupInfo.
    let (epoch0, _) = group_info_row(&log, &group_id)
        .await
        .expect("epoch-0 GroupInfo should exist after group creation");
    assert_eq!(epoch0, 0, "fresh group's GroupInfo should be at epoch 0");

    // Simulate the create-time publish having been dropped by a transient DS
    // failure: remove the only GroupInfo row. This is the exact bricked state.
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

    // An ordinary touch — the same commit-processing pass the sweep, send, and
    // realtime ingest all run. No commit is applied (sole-member group), so the
    // old `any_applied` republish would NOT fire; only the new durability backstop
    // does.
    alice.process_commits_for(&channel_id).await;

    // The heal republished the current-epoch GroupInfo, so a member can once
    // again external-join the group.
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
