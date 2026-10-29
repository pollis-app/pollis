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
//! Telescoping: `Extend` the furthest hop, then run an onion layer ([`crate::onion`]) through it.
//!
//! Path selection is policy and lives in `pollis-core`'s `net::overlay`.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use rustls::pki_types::CertificateDer;

use crate::client::{ClientIdentity, RelayLink};
use crate::onion;
use crate::proto::{self, Connect, Extend};
use crate::stream::BoxedStream;

/// Hard ceiling on circuit length, so a bad path source cannot ask for an unbounded chain.
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

    /// The two shapes a circuit may never have: none at all, and more than the
    /// cap. Checked both when a circuit is built and again before it dials.
    fn validate_hops(hops: &[Hop]) -> anyhow::Result<()> {
        if hops.is_empty() {
            anyhow::bail!("circuit has no hops");
        }
        if hops.len() > MAX_HOPS {
            anyhow::bail!("circuit of {} hops exceeds the {MAX_HOPS}-hop cap", hops.len());
        }
        Ok(())
    }

    /// An n-hop circuit along `hops`, client outward; the last must be first-party (the caller's invariant).
    pub fn build(hops: Vec<Hop>, identity: Arc<ClientIdentity>) -> anyhow::Result<Circuit> {
        Self::validate_hops(&hops)?;
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
    pub async fn connect(&self, target_host: &str, target_port: u16) -> anyhow::Result<BoxedStream> {
        // Re-checked: `hops` may have been reshaped since construction.
        Self::validate_hops(&self.hops)?;

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

        for i in 0..self.hops.len() {
            // Authenticate to this hop. Every hop verifies the device cert
            // offline, so each gets its own freshly-nonced handshake.
            let handshake = self.identity.fresh_handshake()?;
            proto::write_handshake(&mut layer, &handshake, version).await?;

            match self.hops.get(i + 1) {
                // Not the last hop: extend onward and wrap in a layer only the next hop reads.
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

/// Produces a byte pipe to a target through the overlay.
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

/// A factory that rebuilds the same fixed path per call.
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
