//! Circuit abstraction: an ordered list of hops from client to destination.
//!
//! One hop is the v0 path (design §6.1); more is the v1 onion path (§6.2),
//! built by telescoping:
//!
//! ```text
//! Client ─►[Hop 1]─►[Hop 2]─►[Hop 3]─► destination
//!          sees IP,  sees only  sees dest,
//!          not dest  neighbours not IP
//! ```
//!
//! Authenticate to the furthest hop so far, send it `Extend(next)`, then run an
//! onion layer ([`crate::onion`]) to the next hop through it. Path selection is
//! policy and lives in `pollis-core`'s `net::overlay` / `net::directory`.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use rustls::pki_types::CertificateDer;

use crate::client::{ClientIdentity, RelayLink};
use crate::onion;
use crate::proto::{self, Connect, Extend};
use crate::stream::BoxedStream;

/// Hard ceiling on circuit length, so a bad path source cannot ask for an
/// unbounded chain (the design needs at most three).
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

    /// SHA-256 of the leaf cert: the pin carried in the `Extend` naming this hop.
    pub fn cert_fingerprint(&self) -> [u8; 32] {
        proto::cert_fingerprint(self.relay_cert.as_ref())
    }
}

#[derive(Clone)]
pub struct Circuit {
    hops: Vec<Hop>,
    identity: Arc<ClientIdentity>,
}

impl Circuit {
    pub fn build_single_hop(hop: Hop, identity: Arc<ClientIdentity>) -> Circuit {
        Circuit {
            hops: vec![hop],
            identity,
        }
    }

    fn validate_hops(hops: &[Hop]) -> anyhow::Result<()> {
        if hops.is_empty() {
            anyhow::bail!("circuit has no hops");
        }
        if hops.len() > MAX_HOPS {
            anyhow::bail!("circuit of {} hops exceeds the {MAX_HOPS}-hop cap", hops.len());
        }
        Ok(())
    }

    /// An n-hop circuit, client outward. The last hop must be first-party: path
    /// selection's invariant, backed by each relay's destination allowlist.
    pub fn build(hops: Vec<Hop>, identity: Arc<ClientIdentity>) -> anyhow::Result<Circuit> {
        Self::validate_hops(&hops)?;
        Ok(Circuit { hops, identity })
    }

    pub fn len(&self) -> usize {
        self.hops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }

    /// Open a byte pipe through the circuit; the stream carries one onion layer
    /// per hop past the first.
    pub async fn connect(&self, target_host: &str, target_port: u16) -> anyhow::Result<BoxedStream> {
        // Re-checked so an over-long circuit fails before it dials, not halfway.
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
            let handshake = self.identity.fresh_handshake()?;
            proto::write_handshake(&mut layer, &handshake, version).await?;

            match self.hops.get(i + 1) {
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

/// Rebuilds the same fixed path per call.
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
