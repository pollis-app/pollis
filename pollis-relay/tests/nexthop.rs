//! `Extend` may only reach relays the signed directory published (#813).
//!
//! Before this, `Extend` was an arbitrary-dial primitive: the frame carries a
//! bare `SocketAddr` and a SHA-256 the client also chooses, so any authenticated
//! client could make a first-party relay — or any volunteer's laptop, which runs
//! the same `RelayServer` — open a QUIC connection to any address it named. The
//! pinned fingerprint bounds who may talk *back*; it never bounded who gets
//! *dialled*, and the packet leaving the node is the whole primitive.
//!
//! Each test here pins one refusal class, and the ones that matter most assert
//! it the only way that cannot be argued with: a UDP socket at the address the
//! client named receives **nothing**.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey as Ed25519SigningKey};
use ml_dsa::{Keypair, MlDsa44, SigningKey};
use pollis_relay::circuit::{Circuit, Hop};
use pollis_relay::client::ClientIdentity;
use pollis_relay::nexthop::NextHopDirectory;
use pollis_relay::proto::{self, DeviceCertMaterial};
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer, RelayStats};
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const USER: &str = "u_nexthop";
const DEVICE: &str = "d_nexthop";
const ISSUED_AT: u64 = 1_700_000_000;
const DEVICE_ED_PUB: [u8; 32] = [7u8; 32];

// ---- client ----------------------------------------------------------------

fn client_identity() -> Arc<ClientIdentity> {
    let device = SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into());
    let account = SigningKey::<MlDsa44>::from_seed(&[3u8; 32].into());
    let cert = DeviceCertMaterial::mint(
        &account,
        DEVICE,
        &DEVICE_ED_PUB,
        &device.verifying_key().encode(),
        1,
        ISSUED_AT,
    );
    Arc::new(ClientIdentity::new(USER, DEVICE, DEVICE_ED_PUB, device, cert))
}

// ---- the test pool's signed artifacts --------------------------------------

fn directory_key() -> Ed25519SigningKey {
    Ed25519SigningKey::from_bytes(&[55u8; 32])
}

fn pinned_key_b64() -> String {
    B64.encode(directory_key().verifying_key().to_bytes())
}

/// A revocation store holding a freshly-signed list that revokes nobody — the
/// production shape of a middle hop, so that revocation is never what refuses
/// an `Extend` in this file.
fn healthy_revocations() -> pollis_relay::policy::RevocationStore {
    let now = proto::now_unix();
    let payload = format!(
        r#"{{"version":1,"type":"pollis-relay-revocations","seq":1,"issued_at":{now},"expires_at":{},"revoked":[]}}"#,
        now + 300
    );
    let envelope = serde_json::json!({
        "payload_b64": B64.encode(payload.as_bytes()),
        "signature_b64": B64.encode(directory_key().sign(payload.as_bytes()).to_bytes()),
    });
    let store = pollis_relay::policy::RevocationStore::enforcing(pinned_key_b64());
    store
        .install(&serde_json::to_vec(&envelope).unwrap(), now, 0)
        .expect("a freshly-signed empty list installs");
    store
}

/// A directory store listing `members` — each an address and the DER leaf the
/// pool pins for it (empty where the test does not care).
fn directory_of(members: &[(SocketAddr, Vec<u8>)]) -> NextHopDirectory {
    let store = NextHopDirectory::enforcing(pinned_key_b64());
    install_directory(&store, members, 3600);
    store
}

fn install_directory(store: &NextHopDirectory, members: &[(SocketAddr, Vec<u8>)], ttl: i64) {
    let now = proto::now_unix();
    let relays = members
        .iter()
        .map(|(addr, der)| format!(r#"{{"addr":"{addr}","cert_b64":"{}"}}"#, B64.encode(der)))
        .collect::<Vec<_>>()
        .join(",");
    let payload = format!(
        r#"{{"version":1,"type":"pollis-relay-directory","issued_at":{now},"expires_at":{},"relays":[{relays}]}}"#,
        now + ttl
    );
    let envelope = serde_json::json!({
        "payload_b64": B64.encode(payload.as_bytes()),
        "signature_b64": B64.encode(directory_key().sign(payload.as_bytes()).to_bytes()),
    });
    store
        .install(&serde_json::to_vec(&envelope).unwrap(), now)
        .expect("a freshly-signed directory installs");
}

// ---- relays ----------------------------------------------------------------

struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    _task: tokio::task::JoinHandle<()>,
}

impl TestRelay {
    fn hop(&self) -> Hop {
        Hop::new(self.addr, self.cert.clone())
    }
}

/// Spawn a relay on loopback. `tune` gets the config before it is spawned, which
/// is where each test installs the directory it wants this node to enforce.
fn spawn_relay(allow: &[&str], tune: impl FnOnce(&mut RelayConfig)) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().map(|h| format!("{h}:*"))),
    )
    .unwrap();
    config.revocations = healthy_revocations();
    tune(&mut config);

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

/// A loopback TCP echo server: the circuit's destination.
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

/// A UDP socket that answers nothing and counts every datagram it is sent.
///
/// This is the victim of the finding: if the relay dials what the client named,
/// a QUIC Initial lands here. The counter is the proof, not a log line.
struct Blackhole {
    addr: SocketAddr,
    packets: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

async fn spawn_blackhole() -> Blackhole {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let packets = Arc::new(AtomicUsize::new(0));
    let seen = packets.clone();
    let task = tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        while let Ok(_n) = socket.recv(&mut buf).await {
            seen.fetch_add(1, Ordering::Relaxed);
        }
    });
    Blackhole {
        addr,
        packets,
        _task: task,
    }
}

impl Blackhole {
    fn was_dialed(&self) -> bool {
        self.packets.load(Ordering::Relaxed) > 0
    }
}

/// Ask `entry` to extend to `next_addr` (pinned to `next_cert`) and then reach
/// `dest`. Returns the error text, asserting the circuit did not come up.
async fn expect_extend_refused(
    entry: &TestRelay,
    next_addr: SocketAddr,
    next_cert: CertificateDer<'static>,
    dest: SocketAddr,
) -> String {
    let circuit = Circuit::build(
        vec![entry.hop(), Hop::new(next_addr, next_cert)],
        client_identity(),
    )
    .unwrap();
    match circuit.connect(&dest.ip().to_string(), dest.port()).await {
        Ok(_) => panic!("the relay must refuse to extend to {next_addr}"),
        Err(e) => format!("{e:?}"),
    }
}

// ---- the allowed case ------------------------------------------------------

/// The property the refusals must not break: a next hop the signed directory
/// lists is extended to, and the circuit carries bytes end to end.
#[tokio::test]
async fn a_directory_listed_relay_is_extended_to() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    // The exit is spawned first so its address can be published, then the entry
    // node is built enforcing a directory that lists it.
    let exit = spawn_relay(&[host.as_str()], |config| {
        config.allow_private_next_hops = true;
    });
    let entry = spawn_relay(&[], |config| {
        config.next_hops = directory_of(&[(exit.addr, Vec::new())]);
        config.allow_private_next_hops = true;
    });

    let circuit = Circuit::build(vec![entry.hop(), exit.hop()], client_identity()).unwrap();
    let mut stream = circuit.connect(&host, echo_addr.port()).await.unwrap();
    stream.write_all(b"through the pool").await.unwrap();
    let mut buf = [0u8; 16];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"through the pool");

    assert_eq!(entry.stats.extends(), 1, "the listed hop must be extended to");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
}

// ---- the refusal classes ---------------------------------------------------

/// The finding itself: an address the directory does not list is never dialled.
/// The socket at that address is the witness — it receives nothing at all.
#[tokio::test]
async fn an_address_the_directory_does_not_list_is_never_dialed() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let victim = spawn_blackhole().await;

    // The entry node knows a pool of exactly one relay, and it is not the victim.
    let known = spawn_relay(&[], |_| {});
    let entry = spawn_relay(&[], |config| {
        config.next_hops = directory_of(&[(known.addr, Vec::new())]);
        // Loopback is permitted here, so the ONLY thing that can refuse this
        // extend is "the pool never published that address".
        config.allow_private_next_hops = true;
    });

    let started = Instant::now();
    let err = expect_extend_refused(&entry, victim.addr, known.cert.clone(), echo_addr).await;
    assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");

    // Give a dial that should never have happened time to land anyway.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !victim.was_dialed(),
        "the relay opened a QUIC connection to an address no directory published"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the refusal must happen before any dial, not after it times out"
    );
    assert_eq!(entry.stats.extends(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

/// Loopback, RFC1918, link-local and friends are refused even when a directory
/// lists them — the bound on a mis-signed (or lab-copied) directory.
#[tokio::test]
async fn a_private_address_is_refused_even_when_the_directory_lists_it() {
    let (echo_addr, _echo_conns) = spawn_echo().await;
    let victim = spawn_blackhole().await;

    let entry = spawn_relay(&[], |config| {
        // Listed in the signed directory AND on loopback: only the address-class
        // rule can refuse it.
        config.next_hops = directory_of(&[(victim.addr, Vec::new())]);
        config.allow_private_next_hops = false;
    });

    let err = expect_extend_refused(&entry, victim.addr, entry.cert.clone(), echo_addr).await;
    assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!victim.was_dialed(), "a private address must never be dialled");
    assert_eq!(entry.stats.extends(), 0);
}

/// A node with no pinned directory key cannot evaluate the directory, and
/// "cannot evaluate" is not permission — it simply stops being a middle hop.
/// This is the volunteer-device default.
#[tokio::test]
async fn a_node_with_no_directory_refuses_every_extend() {
    let (echo_addr, _echo_conns) = spawn_echo().await;
    let victim = spawn_blackhole().await;

    let entry = spawn_relay(&[], |config| {
        // Everything else about this node says yes: extends are on, loopback is
        // allowed, revocation is current. Only the directory is missing.
        config.allow_private_next_hops = true;
        assert!(!config.next_hops.is_configured(), "the default must be unconfigured");
    });

    let err = expect_extend_refused(&entry, victim.addr, entry.cert.clone(), echo_addr).await;
    assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!victim.was_dialed());
    assert_eq!(entry.stats.extends(), 0);
}

/// A directory that has lapsed is not evidence any more. Checked at USE time, so
/// a node that stops refreshing stops extending rather than running on a
/// membership list that may be hours out of date.
#[tokio::test]
async fn an_expired_directory_stops_the_node_extending() {
    let (echo_addr, _echo_conns) = spawn_echo().await;
    let victim = spawn_blackhole().await;

    let entry = spawn_relay(&[], |config| {
        let directory = NextHopDirectory::enforcing(pinned_key_b64());
        // Valid for one second, and its whole life is spent before the client
        // arrives.
        install_directory(&directory, &[(victim.addr, Vec::new())], 1);
        config.next_hops = directory;
        config.allow_private_next_hops = true;
    });
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let err = expect_extend_refused(&entry, victim.addr, entry.cert.clone(), echo_addr).await;
    assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");
    assert!(!victim.was_dialed(), "an expired directory permits nothing");
}

/// The directory entry pinned to this node's own leaf names this node. A hop to
/// ourselves is a loop, so it is refused even though it is unimpeachably "in the
/// directory".
#[tokio::test]
async fn a_node_will_not_extend_to_itself() {
    let (echo_addr, _echo_conns) = spawn_echo().await;

    // Two-step: the identity has to exist before the directory can pin it.
    let identity = pollis_relay::tls::generate_self_signed(pollis_relay::tls::RELAY_SERVER_NAME)
        .unwrap();
    let own_der = identity.cert_der.as_ref().to_vec();
    let mut config = RelayConfig::with_identity(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::default(),
        identity,
    );
    config.revocations = healthy_revocations();
    config.allow_private_next_hops = true;
    let directory = NextHopDirectory::enforcing(pinned_key_b64());
    config.next_hops = directory.clone();
    let cert = config.server_cert();
    let stats = config.stats.clone();
    let (task, addr) = RelayServer::spawn(config).unwrap();
    let entry = TestRelay {
        addr,
        cert,
        stats,
        _task: task,
    };
    // Now the node is bound, publish it as the pool member it is.
    install_directory(&directory, &[(addr, own_der)], 3600);

    let err = expect_extend_refused(&entry, entry.addr, entry.cert.clone(), echo_addr).await;
    assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");
    assert_eq!(entry.stats.extends(), 0);
}

/// A listed next hop that swallows packets costs one refusal after the deadline,
/// not a task and a circuit slot held for as long as the client cares to wait.
#[tokio::test]
async fn a_next_hop_that_never_answers_is_given_up_on() {
    let (echo_addr, _echo_conns) = spawn_echo().await;
    // A UDP socket that reads and never replies: the QUIC handshake to it can
    // only ever hang.
    let silent = spawn_blackhole().await;

    let entry = spawn_relay(&[], |config| {
        config.next_hops = directory_of(&[(silent.addr, Vec::new())]);
        config.allow_private_next_hops = true;
        config.extend_dial_timeout = Duration::from_millis(300);
    });

    let started = Instant::now();
    let err = expect_extend_refused(&entry, silent.addr, entry.cert.clone(), echo_addr).await;
    assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the extend must be abandoned on its own deadline, not the client's patience"
    );
    // It IS a listed relay, so it was legitimately dialled — the deadline is what
    // ends it.
    assert!(silent.was_dialed());
    assert_eq!(entry.stats.extends(), 0);
}

/// The relay counts a refusal for every class, so an operator sees "clients are
/// naming hops I will not dial" instead of silence.
#[tokio::test]
async fn every_refusal_is_counted_and_none_reaches_the_destination() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let victim = spawn_blackhole().await;
    let entry = spawn_relay(&[], |config| {
        config.next_hops = directory_of(&[("203.0.113.7:9444".parse().unwrap(), Vec::new())]);
        config.allow_private_next_hops = true;
    });

    let before = entry.stats.rejected();
    let _ = expect_extend_refused(&entry, victim.addr, entry.cert.clone(), echo_addr).await;
    assert!(entry.stats.rejected() > before, "the refusal must be recorded");
    assert_eq!(entry.stats.dials(), 0);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
    assert!(!victim.was_dialed());
}
