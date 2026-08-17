//! **Relay-side revocation enforcement at the call site** (#813 Phase C, wired).
//!
//! `pollis_relay::policy` has thorough unit tests for the store's decisions. This
//! file tests the thing those cannot: that the relay *asks*. A store that returns
//! the right verdict and is never consulted is decoration, and that is exactly
//! the state this code shipped in until the `serve_extend` call site existed.
//!
//! Each test is written so the permissive version of the rule fails it:
//!
//! - R1: an **unkeyed** node refuses to extend even with `allow_extend = true` —
//!   "cannot evaluate" resolves the same way as "revoked".
//! - R2: a next hop revoked by **address** is never even dialed. Proved
//!   structurally: the next hop's peer observer records nobody.
//! - R3: a next hop revoked by **cert digest** — with its address deliberately
//!   NOT revoked — is refused *after* the QUIC handshake, so the identity check
//!   runs on the cert the peer actually presented, not just on what the `Extend`
//!   frame claimed. It is contacted and still never serves a layer.
//! - R4: a list that has **expired since it was installed** stops admitting,
//!   without anyone reinstalling anything (staleness is a use-time property).
//! - R5: the check is **selective**, not blanket: a list revoking some other
//!   relay still lets a healthy circuit through.
//! - R6: revocation is **live** — installing a newer list on a running node stops
//!   the next extend.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use ml_dsa::{Keypair, MlDsa44};
use pollis_relay::circuit::{Circuit, Hop};
use pollis_relay::client::ClientIdentity;
use pollis_relay::policy::RevocationStore;
use pollis_relay::proto::{self, DeviceCertMaterial};
use pollis_relay::server::{Allowlist, PeerObserver, RelayConfig, RelayServer, RelayStats};
use pollis_relay::stream::BoxedStream;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const USER: &str = "u_revoke";
const DEVICE: &str = "d_revoke";
const DEVICE_ED_PUB: [u8; 32] = [7u8; 32];

fn client_identity() -> Arc<ClientIdentity> {
    let device = ml_dsa::SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into());
    let account = ml_dsa::SigningKey::<MlDsa44>::from_seed(&[3u8; 32].into());
    let cert = DeviceCertMaterial::mint(
        &account,
        DEVICE,
        &DEVICE_ED_PUB,
        &device.verifying_key().encode(),
        1,
        1_700_000_000,
    );
    Arc::new(ClientIdentity::new(
        USER,
        DEVICE,
        DEVICE_ED_PUB,
        device,
        cert,
    ))
}

// ---- signed revocation artifacts -------------------------------------------

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn directory_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn pinned(sk: &SigningKey) -> String {
    B64.encode(sk.verifying_key().to_bytes())
}

/// Sign a revocation payload exactly as the reconciler does: over the payload
/// bytes, no canonicalization.
fn envelope(sk: &SigningKey, payload: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "payload_b64": B64.encode(payload.as_bytes()),
        "signature_b64": B64.encode(sk.sign(payload.as_bytes()).to_bytes()),
    }))
    .unwrap()
}

fn payload(seq: u64, ttl_secs: i64, revoked: &str) -> String {
    let now = proto::now_unix();
    format!(
        r#"{{"version":1,"type":"pollis-relay-revocations","seq":{seq},"issued_at":{now},"expires_at":{},"revoked":[{revoked}]}}"#,
        now + ttl_secs
    )
}

/// A store keyed on the directory key with `revoked` installed at `seq`.
fn store_revoking(seq: u64, ttl_secs: i64, revoked: &str) -> RevocationStore {
    let sk = directory_key();
    let store = RevocationStore::enforcing(pinned(&sk));
    store
        .install(
            &envelope(&sk, &payload(seq, ttl_secs, revoked)),
            proto::now_unix(),
            0,
        )
        .expect("a freshly-signed list installs");
    store
}

fn healthy_store() -> RevocationStore {
    store_revoking(1, 300, "")
}

/// The `cert_sha256_b64` selector for a relay's DER leaf.
fn cert_selector(cert: &CertificateDer<'static>) -> String {
    use sha2::{Digest, Sha256};
    B64.encode(Sha256::digest(cert.as_ref()))
}

// ---- test relays -----------------------------------------------------------

struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl TestRelay {
    fn hop(&self) -> Hop {
        Hop::new(self.addr, self.cert.clone())
    }

    fn was_contacted(&self) -> bool {
        !self.peers.lock().unwrap().is_empty()
    }
}

fn spawn_relay(allow: &[&str], revocations: RevocationStore) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().copied()),
    )
    .unwrap();
    config.revocations = revocations;

    let peers: Arc<Mutex<Vec<SocketAddr>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = peers.clone();
    let observer: PeerObserver = Arc::new(move |addr: SocketAddr| {
        sink.lock().unwrap().push(addr);
    });
    config.peer_observer = Some(observer);

    let cert = config.server_cert();
    let stats = config.stats.clone();
    let (task, addr) = RelayServer::spawn(config).unwrap();
    TestRelay {
        addr,
        cert,
        stats,
        peers,
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

/// Assert a circuit attempt failed with the typed `ExtendFailed` rejection
/// rather than succeeding, being dropped, or timing out.
///
/// Takes the whole `Result` because `BoxedStream` is deliberately not `Debug` —
/// a byte pipe has nothing safe to print.
fn assert_extend_failed(result: anyhow::Result<BoxedStream>, context: &str) {
    let err = match result {
        Ok(_) => panic!("{context}"),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("ExtendFailed"),
        "expected a typed ExtendFailed rejection, got: {msg}"
    );
}

// ---- R1 --------------------------------------------------------------------

/// A node that was never given the pinned directory key refuses to extend, even
/// though `allow_extend` is true and the next hop is perfectly healthy.
///
/// This is the promise `RevocationStore::unconfigured`'s doc makes ("cannot
/// extend circuits to other relays") and the one that was not true until the
/// call site existed. The named next hop is never contacted at all.
#[tokio::test]
async fn r1_an_unkeyed_relay_cannot_be_a_middle_hop() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay(&[], RevocationStore::unconfigured());
    let r2 = spawn_relay(&[host.as_str()], healthy_store());

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    assert_extend_failed(
        circuit.connect(&host, echo_addr.port()).await,
        "an unkeyed node must refuse to extend",
    );

    assert_eq!(r1.stats.extends(), 0);
    assert!(r1.stats.rejected() >= 1, "the refusal must be recorded");
    assert!(
        !r2.was_contacted(),
        "the next hop must not be dialed by a node that cannot vouch for it"
    );
    assert_eq!(r2.stats.layers(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- R2 --------------------------------------------------------------------

/// A next hop revoked by ADDRESS is refused before the dial — the seized node
/// never sees a packet.
#[tokio::test]
async fn r2_a_revoked_address_is_never_dialed() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r2 = spawn_relay(&[host.as_str()], healthy_store());
    let revoked = format!(r#"{{"addr":"{}","reason":"seized"}}"#, r2.addr);
    let r1 = spawn_relay(&[], store_revoking(2, 300, &revoked));

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    assert_extend_failed(
        circuit.connect(&host, echo_addr.port()).await,
        "a revoked next hop must be refused",
    );

    assert_eq!(r1.stats.extends(), 0);
    assert!(r1.stats.rejected() >= 1);
    // The structural proof: had the address check been skipped, the QUIC
    // handshake would have registered a peer on the revoked node.
    assert!(
        !r2.was_contacted(),
        "a relay revoked by address must not be contacted at all"
    );
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- R3 --------------------------------------------------------------------

/// A next hop revoked by CERT DIGEST is refused too — and, because its address is
/// deliberately left un-revoked, only the post-handshake check on the cert the
/// peer actually presented can be what catches it.
///
/// That selector is the one that binds per-node and peer-hosted relay identities
/// (§7 / phase D1), where nodes stop sharing one pooled leaf.
#[tokio::test]
async fn r3_a_revoked_cert_is_refused_on_the_identity_it_presents() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r2 = spawn_relay(&[host.as_str()], healthy_store());
    let revoked = format!(r#"{{"cert_sha256_b64":"{}"}}"#, cert_selector(&r2.cert));
    let r1 = spawn_relay(&[], store_revoking(2, 300, &revoked));

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    assert_extend_failed(
        circuit.connect(&host, echo_addr.port()).await,
        "a cert-revoked next hop must be refused",
    );

    assert_eq!(r1.stats.extends(), 0);
    assert!(r1.stats.rejected() >= 1);
    // Contacted — the address selector did not match, so the pre-dial check
    // passed — but the circuit still never got through it.
    assert!(
        r2.was_contacted(),
        "this test only proves anything if the pre-dial check did NOT fire"
    );
    assert_eq!(
        r2.stats.layers(),
        0,
        "the revoked node must never be handed a layer"
    );
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- R4 --------------------------------------------------------------------

/// A list that was fresh when installed and has since expired stops admitting.
/// Nobody reinstalls anything — staleness is enforced where the decision is made.
#[tokio::test]
async fn r4_an_expired_list_stops_extends_at_use_time() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    // Valid for two seconds, revoking nobody. `now_unix()` is whole seconds, so
    // a one-second window could expire between minting and first use.
    let r1 = spawn_relay(&[], store_revoking(1, 2, ""));
    let r2 = spawn_relay(&[host.as_str()], healthy_store());

    // While it is still fresh, the circuit works.
    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    let mut stream = circuit
        .connect(&host, echo_addr.port())
        .await
        .expect("a fresh list admits a healthy next hop");
    stream.write_all(b"fresh!").await.unwrap();
    let mut buf = [0u8; 6];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"fresh!");
    assert_eq!(r1.stats.extends(), 1);
    drop(stream);

    tokio::time::sleep(std::time::Duration::from_millis(2_400)).await;

    // Same node, same list, same next hop — now worthless evidence.
    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    assert_extend_failed(
        circuit.connect(&host, echo_addr.port()).await,
        "an expired list must stop admitting",
    );
    assert_eq!(r1.stats.extends(), 1, "no second extend was honoured");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}

// ---- R5 --------------------------------------------------------------------

/// The check is selective. A list that revokes a DIFFERENT relay still lets a
/// healthy circuit through — otherwise "everything is refused" would pass every
/// other test in this file for the wrong reason.
#[tokio::test]
async fn r5_revoking_someone_else_does_not_break_a_healthy_circuit() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r2 = spawn_relay(&[host.as_str()], healthy_store());
    let bystander = spawn_relay(&[], healthy_store());
    let revoked = format!(
        r#"{{"addr":"{}"}},{{"cert_sha256_b64":"{}"}}"#,
        bystander.addr,
        cert_selector(&bystander.cert)
    );
    let r1 = spawn_relay(&[], store_revoking(3, 300, &revoked));

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    let mut stream = circuit
        .connect(&host, echo_addr.port())
        .await
        .expect("an un-revoked next hop is still usable");
    stream.write_all(b"healthy!").await.unwrap();
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"healthy!");

    assert_eq!(r1.stats.extends(), 1);
    assert_eq!(r2.stats.layers(), 1);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
    assert!(!bystander.was_contacted());
}

// ---- R6 --------------------------------------------------------------------

/// Revocation takes effect on a running node: installing a newer list stops the
/// very next extend, with no restart and no reconnect. That liveness is the whole
/// reason the artifact has a 5-minute TTL instead of the directory's hour.
#[tokio::test]
async fn r6_a_newly_published_revocation_stops_the_next_extend() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let store = healthy_store();
    let r1 = spawn_relay(&[], store.clone());
    let r2 = spawn_relay(&[host.as_str()], healthy_store());

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    let stream = circuit
        .connect(&host, echo_addr.port())
        .await
        .expect("healthy before the revocation");
    assert_eq!(r1.stats.extends(), 1);
    drop(stream);

    // The operator publishes; the node's refresh task installs it.
    let sk = directory_key();
    let revoked = format!(r#"{{"addr":"{}","reason":"seized"}}"#, r2.addr);
    store
        .install(
            &envelope(&sk, &payload(9, 300, &revoked)),
            proto::now_unix(),
            0,
        )
        .expect("a newer list installs");

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    assert_extend_failed(
        circuit.connect(&host, echo_addr.port()).await,
        "the newly revoked hop must be refused",
    );
    assert_eq!(r1.stats.extends(), 1, "no extend after the revocation");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}

// ---- R7 --------------------------------------------------------------------

/// Revocation gates `Extend` only. A node that terminates a circuit still serves
/// it: there is no next hop to vouch for, and refusing here would take the
/// single-hop fleet down for a reason that does not apply to it.
#[tokio::test]
async fn r7_single_hop_is_unaffected_by_an_unkeyed_node() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let relay = spawn_relay(&[host.as_str()], RevocationStore::unconfigured());
    let circuit = Circuit::build_single_hop(relay.hop(), client_identity());
    let mut stream = circuit
        .connect(&host, echo_addr.port())
        .await
        .expect("single hop is unaffected");
    stream.write_all(b"solo").await.unwrap();
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"solo");
    assert_eq!(relay.stats.dials(), 1);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}

// ---- R8: the live-circuit gauge (requested by phase D1) --------------------

/// `live_circuits` is a gauge over admitted streams: it tracks concurrency and it
/// comes back down.
///
/// The second half is the one that makes it worth having — a gauge that only ever
/// climbs is strictly worse than the cumulative counter it supplements, because
/// it looks like a live number and is not one. It is incremented per admitted
/// *stream* (in `serve_authenticated`), which is what "circuit" means here, and
/// released by a `Drop` guard so no error path can leak a count.
#[tokio::test]
async fn r8_the_live_circuit_gauge_rises_and_falls() {
    let (echo_addr, _echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let relay = spawn_relay(&[host.as_str()], RevocationStore::unconfigured());

    assert_eq!(relay.stats.live_circuits(), 0);

    let identity = client_identity();
    let mut streams = Vec::new();
    for _ in 0..3 {
        streams.push(
            pollis_relay::client::RelayClient::connect(
                relay.addr,
                &relay.cert,
                &identity,
                &host,
                echo_addr.port(),
            )
            .await
            .unwrap(),
        );
    }
    assert_eq!(
        relay.stats.live_circuits(),
        3,
        "three concurrent circuits must read as three"
    );

    // Close one: the gauge must follow, not just grow.
    streams.pop();
    wait_for_gauge(&relay, 2).await;

    drop(streams);
    wait_for_gauge(&relay, 0).await;
}

/// Poll the gauge to `want`, bounded. The relay learns a peer went away
/// asynchronously, so this is a settle, not a sleep-and-hope.
async fn wait_for_gauge(relay: &TestRelay, want: i64) {
    for _ in 0..100 {
        if relay.stats.live_circuits() == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "live_circuits settled at {}, expected {want}",
        relay.stats.live_circuits()
    );
}
