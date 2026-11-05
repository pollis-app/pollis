//! Every E2EE `FrameCryptor` owns a libwebrtc `FrameCryptorTransformer`, which runs its own
//! OS thread (named after the class). A voice leave must give that thread back, or every
//! join/leave cycle grows the process by one thread per encrypted track.
//!
//! Linux-only: thread names are read from `/proc/self/task/*/comm`.
#![cfg(all(feature = "media", target_os = "linux"))]

use libwebrtc::{
    audio_source::native::NativeAudioSource,
    media_stream_track::MediaStreamTrack,
    native::frame_cryptor::{
        EncryptionAlgorithm, FrameCryptor, KeyDerivationAlgorithm, KeyProvider, KeyProviderOptions,
    },
    peer_connection_factory::{
        native::PeerConnectionFactoryExt, PeerConnectionFactory, RtcConfiguration,
    },
    prelude::AudioSourceOptions,
};

/// `comm` is truncated to 15 bytes by the kernel.
const TRANSFORMER_THREAD: &str = "FrameCryptorTra";

fn transformer_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .filter_map(|e| e.ok())
        .filter(|e| {
            std::fs::read_to_string(e.path().join("comm"))
                .map(|c| c.trim() == TRANSFORMER_THREAD)
                .unwrap_or(false)
        })
        .count()
}

/// Thread creation/teardown is asynchronous to the call that triggers it.
fn settle(expected: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let n = transformer_threads();
        if n == expected || std::time::Instant::now() > deadline {
            return n;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn key_provider() -> KeyProvider {
    let kp = KeyProvider::new(KeyProviderOptions {
        shared_key: true,
        ratchet_window_size: 16,
        ratchet_salt: b"pollis-test".to_vec(),
        failure_tolerance: -1,
        key_ring_size: 16,
        key_derivation_algorithm: KeyDerivationAlgorithm::PBKDF2,
    });
    kp.set_shared_key(0, vec![7u8; 32]);
    kp
}

#[test]
fn a_sender_cryptor_releases_its_transformer_thread_with_the_peer_connection() {
    let factory = PeerConnectionFactory::default();
    let baseline = transformer_threads();

    let pc = factory
        .create_peer_connection(RtcConfiguration::default())
        .expect("peer connection");
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
    let track = factory.create_audio_track("mic", source);
    let sender = pc
        .add_track(MediaStreamTrack::Audio(track), &["s"])
        .expect("add track");

    let cryptor = FrameCryptor::new_for_rtp_sender(
        &factory,
        "me".into(),
        EncryptionAlgorithm::AesGcm,
        key_provider(),
        sender.clone(),
    );
    cryptor.set_enabled(true);
    assert_eq!(
        settle(baseline + 1),
        baseline + 1,
        "constructing a cryptor starts one transformer thread"
    );

    // What `E2eeManager::cleanup` does on `room.close()`: disable and drop the Rust handle.
    cryptor.set_enabled(false);
    drop(cryptor);
    let after_cryptor_drop = settle(baseline);

    // What the SDK does when the session is torn down.
    pc.close();
    drop(sender);
    drop(pc);
    let after_pc_drop = settle(baseline);

    eprintln!(
        "transformer threads: baseline={baseline} with_cryptor={} after_cryptor_drop={after_cryptor_drop} after_pc_drop={after_pc_drop}",
        baseline + 1
    );
    assert_eq!(
        after_pc_drop, baseline,
        "the transformer thread outlived the cryptor AND its peer connection: that is the per-join leak"
    );
}

/// HS256 LiveKit access token, the shape the DS mints (`pollis-delivery::broker::sign_livekit_token`).
fn dev_token(api_key: &str, api_secret: &str, room: &str, identity: &str) -> String {
    use base64::Engine as _;
    use hmac::{Hmac, Mac};
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let header = b64(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = serde_json::json!({
        "iss": api_key, "sub": identity, "iat": now, "nbf": now, "exp": now + 600, "name": identity,
        "video": { "room": room, "roomJoin": true, "canPublish": true, "canSubscribe": true, "canPublishData": true },
    });
    let payload = b64(claims.to_string().as_bytes());
    let signing_input = format!("{header}.{payload}");
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(api_secret.as_bytes()).unwrap();
    mac.update(signing_input.as_bytes());
    format!("{signing_input}.{}", b64(&mac.finalize().into_bytes()))
}

fn e2ee_room_options() -> livekit::RoomOptions {
    use livekit::e2ee::key_provider::{KeyProvider, KeyProviderOptions};
    use livekit::e2ee::{E2eeOptions, EncryptionType};
    let kp = KeyProvider::with_shared_key(KeyProviderOptions::default(), vec![9u8; 32]);
    let mut options = livekit::RoomOptions::default();
    options.encryption = Some(E2eeOptions { encryption_type: EncryptionType::Gcm, key_provider: kp });
    options
}

async fn cryptors_settle(room: &livekit::Room, expected: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let n = room.e2ee_manager().frame_cryptors().len();
        if n >= expected || std::time::Instant::now() > deadline {
            return n;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// The real join/leave shape against a live SFU: two E2EE participants publish to each other
/// (one sender + one receiver cryptor each), then `room.close()` + drop. Needs
/// `POLLIS_TEST_LIVEKIT_URL` (the lab's `ws://127.0.0.1:7880`, dev key `devkey` / secret `secret`);
/// skipped otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_room_releases_its_transformer_threads() {
    use livekit::prelude::*;
    let Ok(url) = std::env::var("POLLIS_TEST_LIVEKIT_URL") else {
        eprintln!("POLLIS_TEST_LIVEKIT_URL unset; skipping the live-SFU thread test");
        return;
    };
    let api_key = std::env::var("POLLIS_TEST_LIVEKIT_KEY").unwrap_or_else(|_| "devkey".into());
    let api_secret = std::env::var("POLLIS_TEST_LIVEKIT_SECRET").unwrap_or_else(|_| "secret".into());
    let room_name = format!("fc-threads-{}", std::process::id());

    let baseline = transformer_threads();
    let mut after_each_cycle = Vec::new();
    for cycle in 0..3 {
        let (room_a, _events_a) = Room::connect(&url, &dev_token(&api_key, &api_secret, &room_name, "alice"), e2ee_room_options())
            .await
            .expect("alice connects");
        let (room_b, _events_b) = Room::connect(&url, &dev_token(&api_key, &api_secret, &room_name, "bob"), e2ee_room_options())
            .await
            .expect("bob connects");
        let mut tracks = Vec::new();
        for room in [&room_a, &room_b] {
            let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 100);
            let track = LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
            room.local_participant()
                .publish_track(LocalTrack::Audio(track.clone()), livekit::options::TrackPublishOptions::default())
                .await
                .expect("publish");
            tracks.push((track, source));
        }
        let a = cryptors_settle(&room_a, 2).await;
        let b = cryptors_settle(&room_b, 2).await;
        let live = settle(baseline + a + b);
        eprintln!("cycle {cycle}: cryptors a={a} b={b}; transformer threads live={live} (baseline {baseline})");

        let _ = room_a.close().await;
        let _ = room_b.close().await;
        drop(room_a);
        drop(room_b);
        drop(tracks);
        let after = settle(baseline);
        eprintln!("cycle {cycle}: transformer threads after close+drop={after}");
        after_each_cycle.push(after);
    }
    assert_eq!(
        after_each_cycle.last().copied(),
        Some(baseline),
        "transformer threads must return to baseline after every close; per-cycle counts {after_each_cycle:?}"
    );
}
