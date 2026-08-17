//! **Transparency-log account-anchoring, end to end on the wire** (#813 Phase E2).
//!
//! `pollis_relay::anchor` unit-tests the verifier. This file tests the relay:
//! that it asks, that it binds the answer to the handshake in front of it, and
//! that nothing about the frame lets a client be better off lying than staying
//! silent.
//!
//! Written so the permissive version of each rule fails:
//!
//! - E1: a node that requires an anchor refuses a client with none, cleanly, and
//!   dials nothing.
//! - E2: a genuine anchor is accepted and the circuit carries bytes.
//! - E3: **the binding.** A head and an inclusion proof that are entirely genuine
//!   — signed by the real log key, for a leaf really in the tree — are refused
//!   when the leaf is not *this* handshake's account. Without this check any
//!   published leaf would anchor any handshake, and anchors are public data.
//! - E4: a self-consistent tree signed by somebody else's key is refused.
//! - E5: a genuinely-signed head from **another tree** (same key, different
//!   domain context) cannot anchor an account.
//! - E6: presenting a bad anchor is never better than presenting none — under a
//!   node that only *verifies*, a forged anchor is still fatal.
//! - E7: an unkeyed node makes no claim: it neither requires nor judges.
//! - E8: a v3 stream cannot present an anchor at all.
//! - E9: two anchors on one stream are a malformed exchange, not a re-check.
//! - E10: the configured head-age bound is really plumbed through.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ml_dsa::{Keypair, MlDsa44};
use pollis_relay::anchor::{
    AccountAnchor, AccountKeyLeaf, AnchorPolicy, ACCOUNT_KEYS_STH_CONTEXT, ACCOUNT_KEY_TENANT,
};
use pollis_relay::client::{ClientIdentity, RelayLink};
use pollis_relay::policy::RevocationStore;
use pollis_relay::proto::{self, Connect, DeviceCertMaterial, ProtoError, RejectReason};
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer, RelayStats};
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use verifiable_log::{Entry, Sth, VerifiableLog};

const USER: &str = "u_anchor";
const DEVICE: &str = "d_anchor";
const DEVICE_ED_PUB: [u8; 32] = [7u8; 32];
const IDENTITY_VERSION: u32 = 1;

fn account_key() -> ml_dsa::SigningKey<MlDsa44> {
    ml_dsa::SigningKey::<MlDsa44>::from_seed(&[3u8; 32].into())
}

/// The exact 1312 bytes the handshake carries as `account_id_pub`.
fn account_id_pub() -> Vec<u8> {
    account_key().verifying_key().encode().to_vec()
}

fn client_identity() -> Arc<ClientIdentity> {
    let device = ml_dsa::SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into());
    let cert = DeviceCertMaterial::mint(
        &account_key(),
        DEVICE,
        &DEVICE_ED_PUB,
        &device.verifying_key().encode(),
        IDENTITY_VERSION,
        1_700_000_000,
    );
    Arc::new(ClientIdentity::new(USER, DEVICE, DEVICE_ED_PUB, device, cert))
}

// ---- the transparency log --------------------------------------------------

fn log_key(seed: u8) -> verifiable_log::SigningKey {
    verifiable_log::SigningKey::from_seed(&[seed; 32].into())
}

fn pinned(sk: &verifiable_log::SigningKey) -> String {
    hex::encode(sk.verifying_key().encode())
}

fn leaf(user_id: &str, identity_version: u64, account_pub: &[u8], seq: i64) -> Vec<u8> {
    serde_json::to_vec(&AccountKeyLeaf {
        user_id: user_id.to_string(),
        identity_version,
        account_id_pub: hex::encode(account_pub),
        seq,
    })
    .unwrap()
}

/// The published tree: some other accounts, then ours.
fn published_leaves() -> Vec<Vec<u8>> {
    vec![
        leaf("u_someone", 1, &[0xaa; 16], 1),
        leaf(USER, u64::from(IDENTITY_VERSION), &account_id_pub(), 2),
        leaf("u_another", 4, &[0xbb; 16], 3),
    ]
}

/// Build a real tree over `leaves` and return an anchor for index `index`, under
/// a head signed by `sk` in `context` at `timestamp_ms`.
fn anchor_for(
    sk: &verifiable_log::SigningKey,
    leaves: &[Vec<u8>],
    index: usize,
    context: &[u8],
    timestamp_ms: u64,
) -> AccountAnchor {
    let mut log = VerifiableLog::new();
    for bytes in leaves {
        log.append(Entry::new(ACCOUNT_KEY_TENANT, bytes.clone())).unwrap();
    }
    let size = log.size() as u64;
    let sth = Sth::create_with_context(sk, size, log.root(), timestamp_ms, context);
    let proof = log.inclusion_proof(index).unwrap();
    AccountAnchor {
        leaf_bytes: leaves[index].clone(),
        leaf_index: proof.leaf_index,
        tree_size: size,
        audit_path: proof
            .audit_path
            .iter()
            .map(|h| <[u8; 32]>::try_from(hex::decode(h).unwrap().as_slice()).unwrap())
            .collect(),
        sth_root: <[u8; 32]>::try_from(hex::decode(&sth.root_hash).unwrap().as_slice()).unwrap(),
        sth_timestamp_ms: timestamp_ms,
        sth_signature: hex::decode(&sth.signature).unwrap(),
    }
}

fn now_ms() -> u64 {
    proto::now_unix().max(0) as u64 * 1000
}

/// A genuine, correctly-bound anchor for our account.
fn genuine_anchor(sk: &verifiable_log::SigningKey) -> AccountAnchor {
    anchor_for(sk, &published_leaves(), 1, ACCOUNT_KEYS_STH_CONTEXT, now_ms())
}

// ---- test relays -----------------------------------------------------------

struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    _task: tokio::task::JoinHandle<()>,
}

fn spawn_relay(allow: &[&str], anchor: AnchorPolicy) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().copied()),
    )
    .unwrap();
    config.anchor = anchor;
    // Anchoring gates clients, revocation gates next hops; these are single-hop
    // relays, so the store never comes into it.
    config.revocations = RevocationStore::unconfigured();
    let cert = config.server_cert();
    let stats = config.stats.clone();
    let (task, addr) = RelayServer::spawn(config).unwrap();
    TestRelay {
        addr,
        cert,
        stats,
        _task: task,
    }
}

async fn spawn_echo() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            c.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    (addr, count)
}

/// Speak one exchange to `relay`: handshake, then zero or more anchors, then
/// `Connect`. Returns the live pipe on success, or the relay's typed rejection.
///
/// The frames are driven by hand because the client half of the anchor plumbing
/// (sourcing a proof from `verify.pollis.com` and caching it) lives in
/// `pollis-core`, outside this crate — see the coordination note. Driving the
/// wire is also the honest shape for a security test: it can send exchanges no
/// well-behaved client would.
async fn exchange(
    relay: &TestRelay,
    anchors: &[AccountAnchor],
    host: &str,
    port: u16,
) -> Result<pollis_relay::stream::RelayStream, ProtoError> {
    let link = RelayLink::open(relay.addr, &relay.cert).await.unwrap();
    let version = link.version();
    let mut stream = link.open_stream().await.unwrap();
    let identity = client_identity();
    proto::write_handshake(&mut stream, &identity.fresh_handshake().unwrap(), version)
        .await
        .unwrap();
    for anchor in anchors {
        proto::write_anchor(&mut stream, anchor, version).await.unwrap();
    }
    proto::write_connect(
        &mut stream,
        &Connect {
            host: host.to_string(),
            port,
        },
        version,
    )
    .await
    .unwrap();
    proto::read_response(&mut stream).await?;
    Ok(stream)
}

fn assert_rejected(
    result: Result<pollis_relay::stream::RelayStream, ProtoError>,
    want: RejectReason,
) {
    match result {
        Ok(_) => panic!("expected {want:?}, but the relay accepted the exchange"),
        Err(ProtoError::Rejected(got)) => assert_eq!(got, want, "wrong rejection reason"),
        Err(other) => panic!("expected a typed {want:?} rejection, got {other:?}"),
    }
}

// ---- E1 --------------------------------------------------------------------

/// A node that requires an anchor refuses a client that presents none — cleanly,
/// with the actionable reason code, and without dialing anything.
#[tokio::test]
async fn e1_a_required_anchor_is_actually_required() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, true).unwrap(),
    );

    assert_rejected(
        exchange(&relay, &[], &host, echo_addr.port()).await,
        RejectReason::AnchorRequired,
    );
    assert_eq!(relay.stats.dials(), 0, "no destination may be reached");
    assert_eq!(relay.stats.anchors_verified(), 0);
    assert!(relay.stats.rejected() >= 1);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- E2 --------------------------------------------------------------------

/// A genuine anchor is accepted and the circuit carries bytes as usual.
#[tokio::test]
async fn e2_a_genuine_anchor_is_accepted() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, true).unwrap(),
    );

    let mut stream = exchange(&relay, &[genuine_anchor(&sk)], &host, echo_addr.port())
        .await
        .expect("a genuine anchor is accepted");
    stream.write_all(b"anchored").await.unwrap();
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"anchored");

    assert_eq!(relay.stats.anchors_verified(), 1);
    assert_eq!(relay.stats.anchors_refused(), 0);
    assert_eq!(relay.stats.dials(), 1);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}

// ---- E3: the binding -------------------------------------------------------

/// The load-bearing test. Everything about this anchor is genuine — the log key
/// is the real one, the head is really signed, the leaf is really in the tree,
/// the audit path really carries it to the root — and it is still refused,
/// because the leaf is somebody else's account.
///
/// Anchors are public data: the whole tree is downloadable. If the relay checked
/// only "is this a valid proof", any attacker could take any published leaf and
/// anchor their own freshly-minted account key with it, and anchoring would prove
/// nothing at all.
#[tokio::test]
async fn e3_a_genuine_proof_for_another_account_is_refused() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, true).unwrap(),
    );

    // Index 0 and 2 are other users' leaves. Both are genuinely published.
    for index in [0usize, 2] {
        let borrowed = anchor_for(
            &sk,
            &published_leaves(),
            index,
            ACCOUNT_KEYS_STH_CONTEXT,
            now_ms(),
        );
        assert_rejected(
            exchange(&relay, &[borrowed], &host, echo_addr.port()).await,
            RejectReason::Unauthorized,
        );
    }

    // And a leaf naming OUR user id but a different account key — the shape an
    // attacker who wants to keep a stolen user id but swap the key would build.
    let mut leaves = published_leaves();
    leaves[1] = leaf(USER, u64::from(IDENTITY_VERSION), &[0xde; 1312], 2);
    let swapped = anchor_for(&sk, &leaves, 1, ACCOUNT_KEYS_STH_CONTEXT, now_ms());
    assert_rejected(
        exchange(&relay, &[swapped], &host, echo_addr.port()).await,
        RejectReason::Unauthorized,
    );

    assert_eq!(relay.stats.anchors_verified(), 0);
    assert_eq!(relay.stats.anchors_refused(), 3);
    assert_eq!(relay.stats.dials(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- E4 --------------------------------------------------------------------

/// An attacker's own perfectly-self-consistent log does not verify: the relay
/// pins one key and the tree it publishes is not that log.
#[tokio::test]
async fn e4_a_tree_signed_by_another_key_is_refused() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let real = log_key(11);
    let attacker = log_key(12);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&real), 86_400, true).unwrap(),
    );

    // The attacker publishes our real leaf in their own tree, correctly proven.
    assert_rejected(
        exchange(&relay, &[genuine_anchor(&attacker)], &host, echo_addr.port()).await,
        RejectReason::Unauthorized,
    );
    assert_eq!(relay.stats.anchors_refused(), 1);
    assert_eq!(relay.stats.dials(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- E5 --------------------------------------------------------------------

/// One log key signs three trees; only the domain context tells them apart. A
/// genuinely-signed head of the **commit-log** tree must not anchor an account.
#[tokio::test]
async fn e5_a_head_from_another_tree_cannot_anchor_an_account() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, true).unwrap(),
    );

    for context in [
        &b"pollis-verifiable-log:sth:v2"[..],
        &b"pollis-verifiable-log:sth:v2:binaries"[..],
    ] {
        let wrong_tree = anchor_for(&sk, &published_leaves(), 1, context, now_ms());
        assert_rejected(
            exchange(&relay, &[wrong_tree], &host, echo_addr.port()).await,
            RejectReason::Unauthorized,
        );
    }
    assert_eq!(relay.stats.anchors_refused(), 2);
    assert_eq!(relay.stats.dials(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- E6 --------------------------------------------------------------------

/// On a node that only *verifies*, a client with no anchor is served — but a
/// client with a forged one is not. Lying must never be at least as good as
/// staying silent, or every enforcement rung below `Require` would be bypassable
/// by sending garbage.
#[tokio::test]
async fn e6_a_bad_anchor_is_fatal_even_when_one_is_not_required() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, false).unwrap(),
    );

    // No anchor: served, because this node does not require one.
    let stream = exchange(&relay, &[], &host, echo_addr.port())
        .await
        .expect("a client with no anchor is still served here");
    drop(stream);
    assert_eq!(relay.stats.dials(), 1);

    // A forged one: refused, exactly as on a requiring node.
    assert_rejected(
        exchange(
            &relay,
            &[genuine_anchor(&log_key(12))],
            &host,
            echo_addr.port(),
        )
        .await,
        RejectReason::Unauthorized,
    );
    assert_eq!(relay.stats.anchors_refused(), 1);
    assert_eq!(relay.stats.dials(), 1, "the forged exchange dialed nothing");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}

// ---- E7 --------------------------------------------------------------------

/// A node with no log key makes no anchoring claim. It does not require one, and
/// it does not pretend to have judged one it cannot check — so the counters stay
/// at zero either way. This is the shipped default, and it is why turning the
/// feature on is a deployment decision rather than a protocol break.
#[tokio::test]
async fn e7_an_unkeyed_node_neither_requires_nor_judges() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let relay = spawn_relay(&[host.as_str()], AnchorPolicy::Ignore);

    exchange(&relay, &[], &host, echo_addr.port())
        .await
        .expect("no anchor, no key, no problem");
    // Even a nonsense anchor is not judged: this node has no basis to.
    exchange(
        &relay,
        &[genuine_anchor(&log_key(12))],
        &host,
        echo_addr.port(),
    )
    .await
    .expect("an unkeyed node does not reject what it cannot evaluate");

    assert_eq!(relay.stats.anchors_verified(), 0);
    assert_eq!(relay.stats.anchors_refused(), 0);
    assert_eq!(relay.stats.dials(), 2);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 2);
}

// ---- E8 --------------------------------------------------------------------

/// A v3-generation stream cannot present an anchor: the writer refuses to encode
/// one, and a hand-crafted v3 anchor frame is refused by the relay rather than
/// parsed. A v4 concept must not be reachable from a v3 stream in either
/// direction.
#[tokio::test]
async fn e8_a_v3_stream_cannot_present_an_anchor() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, false).unwrap(),
    );

    // The writer will not emit one below v4, and puts no byte on the wire trying.
    let mut buf = Vec::new();
    assert!(
        proto::write_anchor(&mut buf, &genuine_anchor(&sk), proto::MIN_PROTOCOL_VERSION)
            .await
            .is_err()
    );
    assert!(buf.is_empty());

    // Hand-craft the encoding the writer refuses to produce: a v3-versioned
    // MSG_ANCHOR, straight onto the wire.
    let link = RelayLink::open(relay.addr, &relay.cert).await.unwrap();
    let mut stream = link.open_stream().await.unwrap();
    let identity = client_identity();
    let v3 = proto::MIN_PROTOCOL_VERSION;
    proto::write_handshake(&mut stream, &identity.fresh_handshake().unwrap(), v3)
        .await
        .unwrap();
    // Encode the frame at v4, then rewrite the version byte to v3.
    let mut frame = Vec::new();
    proto::write_anchor(&mut frame, &genuine_anchor(&sk), proto::PROTOCOL_VERSION)
        .await
        .unwrap();
    frame[0] = v3;
    stream.write_all(&frame).await.unwrap();
    stream.flush().await.unwrap();

    let err = proto::read_response(&mut stream)
        .await
        .expect_err("a v3 anchor must be refused");
    assert!(
        matches!(err, ProtoError::Rejected(RejectReason::BadRequest)),
        "expected a BadRequest rejection, got {err:?}"
    );
    assert_eq!(relay.stats.anchors_verified(), 0);
    assert_eq!(relay.stats.dials(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- E9 --------------------------------------------------------------------

/// Two anchors on one stream is a malformed exchange. There is no legitimate
/// reason to present a second one, and allowing it would mean a later frame could
/// be used to try to displace an earlier verdict.
#[tokio::test]
async fn e9_two_anchors_on_one_stream_are_malformed() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 86_400, true).unwrap(),
    );

    let good = genuine_anchor(&sk);
    assert_rejected(
        exchange(&relay, &[good.clone(), good], &host, echo_addr.port()).await,
        RejectReason::BadRequest,
    );
    assert_eq!(relay.stats.dials(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- E10 -------------------------------------------------------------------

/// A stale head is refused at the relay, not just in the unit tests — the
/// configured age bound is actually plumbed through.
#[tokio::test]
async fn e10_a_stale_head_is_refused_at_the_relay() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let sk = log_key(11);
    // One hour of tolerance.
    let relay = spawn_relay(
        &[host.as_str()],
        AnchorPolicy::configured(&pinned(&sk), 3_600, true).unwrap(),
    );

    let old = anchor_for(
        &sk,
        &published_leaves(),
        1,
        ACCOUNT_KEYS_STH_CONTEXT,
        now_ms() - 7_200_000,
    );
    assert_rejected(
        exchange(&relay, &[old], &host, echo_addr.port()).await,
        RejectReason::Unauthorized,
    );

    // A head inside the window from the same tree is fine, so the rejection was
    // about age and nothing else.
    exchange(&relay, &[genuine_anchor(&sk)], &host, echo_addr.port())
        .await
        .expect("a fresh head from the same tree is accepted");

    assert_eq!(relay.stats.anchors_refused(), 1);
    assert_eq!(relay.stats.anchors_verified(), 1);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}
