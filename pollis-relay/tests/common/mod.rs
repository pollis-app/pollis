//! Shared fixtures for the relay's integration suites.
//!
//! Compiled into each test binary that declares `mod common;`, so not every
//! helper is used by every suite.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use pollis_relay::nexthop::NextHopDirectory;

/// The test pool's directory-signing key. A fixed seed so a suite can re-sign
/// the directory as it spawns relays.
fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[77u8; 32])
}

/// The signed directory this suite's relays check `Extend` against, plus the
/// addresses published into it so far.
///
/// **Every test relay that will be asked to `Extend` needs to be in it.** A
/// relay only extends to an address the current signed directory lists
/// (`pollis_relay::nexthop`), so a node built without this refuses every
/// multi-hop dial with `ExtendFailed` — the same shape `healthy_revocations`
/// exists for. It is shared and **additive** across the tests in one binary:
/// each suite's relays bind fresh loopback ports, so one test's pool membership
/// is invisible to another's, and re-signing on every spawn keeps the artifact
/// fresh without any test having to sequence it.
fn pool() -> &'static (NextHopDirectory, Mutex<Vec<SocketAddr>>) {
    static POOL: OnceLock<(NextHopDirectory, Mutex<Vec<SocketAddr>>)> = OnceLock::new();
    POOL.get_or_init(|| {
        (
            NextHopDirectory::enforcing(B64.encode(signing_key().verifying_key().to_bytes())),
            Mutex::new(Vec::new()),
        )
    })
}

/// The store to hand a test relay's `RelayConfig::next_hops`.
pub fn test_directory() -> NextHopDirectory {
    pool().0.clone()
}

/// Add `addr` to the test pool and re-sign the directory, so relays already
/// running observe the new member immediately (the store is one shared cell).
pub fn publish_hop(addr: SocketAddr) {
    let (store, members) = pool();
    let mut members = members.lock().unwrap();
    members.push(addr);
    let now = pollis_relay::proto::now_unix();
    let relays = members
        .iter()
        .map(|a| format!(r#"{{"addr":"{a}","cert_b64":""}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let payload = format!(
        r#"{{"version":1,"type":"pollis-relay-directory","issued_at":{now},"expires_at":{},"relays":[{relays}]}}"#,
        now + 3600
    );
    let envelope = serde_json::json!({
        "payload_b64": B64.encode(payload.as_bytes()),
        "signature_b64": B64.encode(signing_key().sign(payload.as_bytes()).to_bytes()),
    });
    store
        .install(&serde_json::to_vec(&envelope).unwrap(), now)
        .expect("a freshly-signed directory installs");
}
