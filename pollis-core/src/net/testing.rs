//! Test-only fixtures shared by the `net` module's suites. Compiled out of every
//! real build (`#[cfg(test)]` at the `mod` site).

/// A revocation store holding a freshly-signed list that revokes nobody.
///
/// **Every test relay that will be asked to `Extend` needs one.** A node that
/// cannot evaluate revocation refuses to extend circuits (#813 phase C, enforced
/// at the `serve_extend` call site by phase E2), so the production shape of a
/// middle hop is "keyed, with a current list" — a relay built without this
/// fails every multi-hop dial with `ExtendFailed`.
///
/// Lives here rather than being copied per-suite because three separate suites
/// need the identical fixture (`net::overlay`, `net::peer::engine`, and
/// `pollis-relay/tests/onion.rs` — the last has its own copy, being in another
/// crate). The unkeyed and genuinely-revoked cases are covered directly in
/// `pollis-relay/tests/revocation.rs`; this is only the "healthy node" shape.
pub(crate) fn healthy_revocations() -> pollis_relay::policy::RevocationStore {
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};

    let sk = SigningKey::from_bytes(&[42u8; 32]);
    let now = pollis_relay::proto::now_unix();
    let payload = format!(
        r#"{{"version":1,"type":"pollis-relay-revocations","seq":1,"issued_at":{now},"expires_at":{},"revoked":[]}}"#,
        now + 300
    );
    let b64 = base64::engine::general_purpose::STANDARD;
    let envelope = serde_json::json!({
        "payload_b64": b64.encode(payload.as_bytes()),
        "signature_b64": b64.encode(sk.sign(payload.as_bytes()).to_bytes()),
    });
    let store =
        pollis_relay::policy::RevocationStore::enforcing(b64.encode(sk.verifying_key().to_bytes()));
    store
        .install(&serde_json::to_vec(&envelope).unwrap(), now, 0)
        .expect("a freshly-signed empty list installs");
    store
}

/// The signed directory this process's test relays check `Extend` against, plus
/// the loopback addresses published into it so far.
///
/// **Every test relay that will be asked to `Extend` needs one**, for the same
/// reason it needs [`healthy_revocations`]: a relay extends only to an address
/// the current signed directory lists (`pollis_relay::nexthop`), so a node built
/// without one refuses every multi-hop dial with `ExtendFailed`.
///
/// Shared and **additive** across the suites in one binary: every test relay
/// binds a fresh loopback port, so one test's pool membership is invisible to
/// another's, and re-signing on each spawn keeps the artifact fresh without any
/// test having to sequence it.
fn test_pool() -> &'static (
    pollis_relay::nexthop::NextHopDirectory,
    std::sync::Mutex<Vec<std::net::SocketAddr>>,
) {
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;

    static POOL: std::sync::OnceLock<(
        pollis_relay::nexthop::NextHopDirectory,
        std::sync::Mutex<Vec<std::net::SocketAddr>>,
    )> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let sk = SigningKey::from_bytes(&[77u8; 32]);
        (
            pollis_relay::nexthop::NextHopDirectory::enforcing(
                base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes()),
            ),
            std::sync::Mutex::new(Vec::new()),
        )
    })
}

/// The store to hand a test relay's `RelayConfig::next_hops` (or
/// `PeerEngine::start`).
pub(crate) fn test_directory() -> pollis_relay::nexthop::NextHopDirectory {
    test_pool().0.clone()
}

/// Add `addr` to the test pool and re-sign the directory, so relays already
/// running observe the new member immediately (the store is one shared cell).
pub(crate) fn publish_hop(addr: std::net::SocketAddr) {
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};

    let sk = SigningKey::from_bytes(&[77u8; 32]);
    let (store, members) = test_pool();
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
    let b64 = base64::engine::general_purpose::STANDARD;
    let envelope = serde_json::json!({
        "payload_b64": b64.encode(payload.as_bytes()),
        "signature_b64": b64.encode(sk.sign(payload.as_bytes()).to_bytes()),
    });
    store
        .install(&serde_json::to_vec(&envelope).unwrap(), now)
        .expect("a freshly-signed directory installs");
}
