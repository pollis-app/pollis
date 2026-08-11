//! Circuit abstraction: an ordered list of hops from client to destination.
//!
//! A [`Circuit`] is `Vec<Hop>`. One hop is the v0 path (design §6.1); two or
//! more is the v1 onion path (§6.2), built by **telescoping**:
//!
//! ```text
//! Client ─►[Hop 1]─►[Hop 2]─►[Hop 3]─► destination
//!          sees IP,  sees only  sees dest,
//!          not dest  neighbours not IP
//! ```
//!
//! Construction, hop by hop: authenticate to the furthest hop reached so far,
//! send it `Extend(next)`, and once it answers `Ok` run an onion layer
//! ([`crate::onion`]) to the next hop *through* it. Every frame the client writes
//! after that is wrapped in one layer per remaining hop, and each relay peels
//! exactly one. Only the last hop ever receives the `Connect` naming the
//! destination, and only the first hop ever sees the client's address.
//!
//! This module owns the *mechanism*. Which relays a circuit is made of — guard
//! pinning, path length, directory selection — is policy and lives in
//! `pollis-core`'s `net::overlay` / `net::directory`.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use rustls::pki_types::CertificateDer;

use crate::client::{ClientIdentity, RelayLink};
use crate::onion;
use crate::proto::{self, Connect, Extend};
use crate::stream::BoxedStream;

/// Hard ceiling on circuit length. Each hop costs a round trip to build and a
/// nested TLS session to carry, and nothing in the design calls for more than
/// three; the cap exists so a bad path source cannot ask for an unbounded chain.
pub const MAX_HOPS: usize = 4;

/// One relay in a circuit: where it is and the cert that identifies it.
#[derive(Clone)]
pub struct Hop {
    pub addr: SocketAddr,
    pub relay_cert: CertificateDer<'static>,
}

impl Hop {
    pub fn new(addr: SocketAddr, relay_cert: CertificateDer<'static>) -> Self {
        Hop { addr, relay_cert }
    }

    /// SHA-256 of this hop's leaf cert — the compact pin carried in the `Extend`
    /// that points the previous hop at it.
    pub fn cert_fingerprint(&self) -> [u8; 32] {
        proto::cert_fingerprint(self.relay_cert.as_ref())
    }
}

/// An ordered path of relay hops plus the client identity used to authenticate
/// to them.
#[derive(Clone)]
pub struct Circuit {
    hops: Vec<Hop>,
    identity: Arc<ClientIdentity>,
}

impl Circuit {
    /// A one-hop circuit to `hop` (design §6.1, the v0 path).
    pub fn build_single_hop(hop: Hop, identity: Arc<ClientIdentity>) -> Circuit {
        Circuit {
            hops: vec![hop],
            identity,
        }
    }

    /// An n-hop circuit along `hops`, in order from the client outward. The last
    /// entry is the hop that talks to the destination and must therefore be a
    /// first-party relay — that is the caller's (path selection's) invariant to
    /// hold; this crate enforces it as the destination allowlist each relay
    /// applies to a `Connect`.
    pub fn build(hops: Vec<Hop>, identity: Arc<ClientIdentity>) -> anyhow::Result<Circuit> {
        if hops.is_empty() {
            anyhow::bail!("circuit has no hops");
        }
        if hops.len() > MAX_HOPS {
            anyhow::bail!("circuit of {} hops exceeds the {MAX_HOPS}-hop cap", hops.len());
        }
        Ok(Circuit { hops, identity })
    }

    /// Number of hops.
    pub fn len(&self) -> usize {
        self.hops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }

    /// Open a byte pipe through the circuit to `target_host:target_port`.
    ///
    /// For `n == 1` this is the v0 exchange verbatim: handshake + connect
    /// pipelined to the one relay, then the raw pipe. For `n > 1` each
    /// intermediate hop is told to `Extend` to its successor and the client runs
    /// a fresh onion layer to that successor before speaking to it, so the
    /// returned stream is wrapped in one encryption layer per hop past the first.
    pub async fn connect(&self, target_host: &str, target_port: u16) -> anyhow::Result<BoxedStream> {
        if self.hops.is_empty() {
            anyhow::bail!("circuit has no hops");
        }
        if self.hops.len() > MAX_HOPS {
            anyhow::bail!(
                "circuit of {} hops exceeds the {MAX_HOPS}-hop cap",
                self.hops.len()
            );
        }

        let first = &self.hops[0];
        let link = RelayLink::open(first.addr, &first.relay_cert).await?;
        let version = link.version();
        if self.hops.len() > 1 && version < proto::ONION_MIN_VERSION {
            anyhow::bail!(
                "relay {} speaks wire v{version}; multi-hop needs v{}",
                first.addr,
                proto::ONION_MIN_VERSION
            );
        }
        let mut layer = BoxedStream::new(link.open_stream().await?);

        for (i, _hop) in self.hops.iter().enumerate() {
            // Authenticate to this hop. Every hop verifies the device cert
            // offline, so each gets its own freshly-nonced handshake.
            let handshake = self.identity.fresh_handshake()?;
            proto::write_handshake(&mut layer, &handshake, version).await?;

            match self.hops.get(i + 1) {
                // Not the last hop: hand the circuit onward, then wrap the pipe
                // in a layer only the next hop can read. Everything after this
                // point is opaque to hop `i`.
                Some(next) => {
                    let extend = Extend {
                        addr: next.addr,
                        next_cert_sha256: next.cert_fingerprint(),
                    };
                    proto::write_extend(&mut layer, &extend, version).await?;
                    proto::read_response(&mut layer).await?;
                    let wrapped = onion::client_layer(layer, &next.relay_cert).await?;
                    layer = BoxedStream::new(wrapped);
                }
                // Last hop: only now does the destination appear on the wire,
                // and it appears inside every layer built above.
                None => {
                    let connect = Connect {
                        host: target_host.to_string(),
                        port: target_port,
                    };
                    proto::write_connect(&mut layer, &connect, version).await?;
                    proto::read_response(&mut layer).await?;
                }
            }
        }

        Ok(layer)
    }
}

/// Produces a byte pipe to a target through the overlay. The shim depends on
/// this rather than a concrete `Circuit` so path selection / rebuild can slot in
/// without touching the shim.
#[async_trait]
pub trait CircuitFactory: Send + Sync {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream>;
}

/// A factory that builds a fresh single-hop circuit to a fixed relay per call.
pub struct SingleHopFactory {
    hop: Hop,
    identity: Arc<ClientIdentity>,
}

impl SingleHopFactory {
    pub fn new(hop: Hop, identity: Arc<ClientIdentity>) -> Self {
        SingleHopFactory { hop, identity }
    }
}

#[async_trait]
impl CircuitFactory for SingleHopFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        let circuit = Circuit::build_single_hop(self.hop.clone(), self.identity.clone());
        circuit.connect(host, port).await
    }
}

/// A factory that rebuilds the same fixed path per call. Useful for tests and
/// for a caller that has already decided its path; a real client picks a fresh
/// path per circuit from the directory, which is `pollis-core`'s job.
pub struct StaticPathFactory {
    hops: Vec<Hop>,
    identity: Arc<ClientIdentity>,
}

impl StaticPathFactory {
    pub fn new(hops: Vec<Hop>, identity: Arc<ClientIdentity>) -> Self {
        StaticPathFactory { hops, identity }
    }
}

#[async_trait]
impl CircuitFactory for StaticPathFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        let circuit = Circuit::build(self.hops.clone(), self.identity.clone())?;
        circuit.connect(host, port).await
    }
}
