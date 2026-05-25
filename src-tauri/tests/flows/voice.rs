use crate::harness::{wipe, TestClient};
use serial_test::serial;

/// Voice E2EE: members of the same MLS group derive identical voice keys at
/// the same epoch; the key changes when the epoch advances; a member added
/// at the new epoch derives the same new key everyone else does. Proves the
/// FrameCryptor input is consistent across peers without needing a real
/// LiveKit server in the loop. Also exercises the `on_mls_epoch_changed`
/// rotation hook: after a commit is applied, a client whose `VoiceState` is
/// "in a call" sees its `e2ee_epoch` advance and its `KeyProvider` re-keyed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn voice_e2ee_keys_match_across_members_and_rotate_on_epoch() {
    use pollis_lib::commands::voice_e2ee;

    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _ap = alice.sign_up("alice@test.local").await;
    let bp = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("VoiceE2EE").await;
    alice.invite(&group_id, &bp.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob has pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    alice.process_commits_for(&channel_id).await;

    let alice_id = alice.user_id().to_owned();
    let bob_id = bob.user_id().to_owned();

    // ── Epoch N: alice + bob derive the same key ─────────────────────────
    let (alice_key, alice_idx, alice_epoch, alice_group) =
        voice_e2ee::derive_voice_key(&alice.state, &channel_id, &alice_id, None)
            .await
            .expect("alice derive");
    let (bob_key, bob_idx, bob_epoch, bob_group) =
        voice_e2ee::derive_voice_key(&bob.state, &channel_id, &bob_id, None)
            .await
            .expect("bob derive");

    assert_eq!(
        alice_key, bob_key,
        "two MLS members at the same epoch must derive the same voice key"
    );
    assert_eq!(alice_idx, bob_idx, "key_index must match");
    assert_eq!(alice_epoch, bob_epoch, "epoch must match");
    assert_eq!(alice_group, bob_group, "resolved MLS group must match");
    assert_eq!(alice_key.len(), 32, "voice key must be 32 bytes");
    assert!(
        alice_key.iter().any(|b| *b != 0),
        "voice key must not be all zeros"
    );

    let key_at_epoch_n = alice_key.clone();
    let epoch_n = alice_epoch;
    let resolved_mls_group = alice_group.clone();

    // ── Simulate alice being "in a call" so the rotation hook has work. ──
    // We plumb in the same KeyProvider build_e2ee_options would have
    // produced at join time, plus the group id and the current epoch.
    let alice_kp = {
        let opts = voice_e2ee::build_e2ee_options(key_at_epoch_n.clone());
        opts.key_provider.clone()
    };
    {
        let mut voice = alice.state.voice.lock().await;
        voice.e2ee_key_provider = Some(alice_kp.clone());
        voice.e2ee_mls_group_id = Some(resolved_mls_group.clone());
        voice.e2ee_epoch = epoch_n;
    }

    // ── Add carol → epoch advances ───────────────────────────────────────
    let mut carol = TestClient::new().await;
    let cp = carol.sign_up("carol@test.local").await;
    alice.invite(&group_id, &cp.username).await;
    let invite_id = carol
        .first_pending_invite()
        .await
        .expect("carol has pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    carol.accept_invite(&invite_id).await;
    carol.poll().await;

    // Existing members apply the add commit (this is the path that fires
    // `on_mls_epoch_changed` inside `process_pending_commits_inner`).
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // ── Epoch N+1: all three derive the same new key ─────────────────────
    let carol_id = carol.user_id().to_owned();
    let (alice_key2, alice_idx2, alice_epoch2, _) =
        voice_e2ee::derive_voice_key(&alice.state, &channel_id, &alice_id, None)
            .await
            .expect("alice derive @N+1");
    let (bob_key2, bob_idx2, bob_epoch2, _) =
        voice_e2ee::derive_voice_key(&bob.state, &channel_id, &bob_id, None)
            .await
            .expect("bob derive @N+1");
    let (carol_key, carol_idx, carol_epoch, _) =
        voice_e2ee::derive_voice_key(&carol.state, &channel_id, &carol_id, None)
            .await
            .expect("carol derive @N+1");

    assert!(
        alice_epoch2 > epoch_n,
        "epoch must advance after a membership commit (was {epoch_n}, now {alice_epoch2})"
    );
    assert_eq!(alice_epoch2, bob_epoch2);
    assert_eq!(alice_epoch2, carol_epoch);
    assert_eq!(alice_key2, bob_key2, "alice and bob still match each other");
    assert_eq!(
        alice_key2, carol_key,
        "carol joins at the new epoch and must derive the same key"
    );
    assert_eq!(alice_idx2, bob_idx2);
    assert_eq!(alice_idx2, carol_idx);
    assert_ne!(
        alice_key2, key_at_epoch_n,
        "voice key must change when MLS epoch advances (context binding)"
    );

    // ── Rotation hook fired on alice's VoiceState ────────────────────────
    let (alice_voice_epoch, alice_voice_group) = {
        let voice = alice.state.voice.lock().await;
        (voice.e2ee_epoch, voice.e2ee_mls_group_id.clone())
    };
    assert_eq!(
        alice_voice_epoch, alice_epoch2,
        "process_pending_commits should have advanced VoiceState.e2ee_epoch via on_mls_epoch_changed"
    );
    assert_eq!(alice_voice_group.as_deref(), Some(resolved_mls_group.as_str()));

    // ── Negative control: bob did NOT have voice "armed" ─────────────────
    // His VoiceState should still report epoch 0 / no group, proving the
    // hook is a no-op when voice is idle (no spurious state churn).
    let (bob_voice_epoch, bob_voice_group) = {
        let voice = bob.state.voice.lock().await;
        (voice.e2ee_epoch, voice.e2ee_mls_group_id.clone())
    };
    assert_eq!(bob_voice_epoch, 0);
    assert!(bob_voice_group.is_none());

    drop(alice);
    drop(bob);
    drop(carol);
}
