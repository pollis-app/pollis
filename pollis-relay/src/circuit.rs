//! Circuit abstraction: an ordered list of hops from client to destination.
//!
//! v0 builds a single-hop circuit, but a [`Circuit`] is modelled as `Vec<Hop>`
//! so v1 multi-hop is a parameter change, not a rewrite. The onion-wrapping seam
//! for `n > 1` is marked below; it is intentionally *not* implemented here.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use rustls::pki_types::CertificateDer;

use crate::client::{ClientIdentity, RelayClient};
use crate::stream::BoxedStream;

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
}

/// An ordered path of relay hops plus the client identity used to authenticate
/// to them.
#[derive(Clone)]
pub struct Circuit {
    hops: Vec<Hop>,
    identity: Arc<ClientIdentity>,
}

impl Circuit {
    /// v0: a one-hop circuit to `hop`.
    pub fn build_single_hop(hop: Hop, identity: Arc<ClientIdentity>) -> Circuit {
        Circuit { hops: vec![hop], identity }
    }

    /// Number of hops. v0 is always 1.
    pub fn len(&self) -> usize {
        self.hops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }

    /// Open a byte pipe through the circuit to `target_host:target_port`.
    ///
    /// For `n == 1` this delegates to [`RelayClient`]. For `n > 1` the onion
    /// seam is here: wrap the `Connect` in one encryption layer per hop and
    /// dial hop 1, each relay peeling one layer. Not implemented in v0.
    pub async fn connect(&self, target_host: &str, target_port: u16) -> anyhow::Result<BoxedStream> {
        match self.hops.len() {
            0 => anyhow::bail!("circuit has no hops"),
            1 => {
                let hop = &self.hops[0];
                let stream = RelayClient::connect(
                    hop.addr,
                    &hop.relay_cert,
                    &self.identity,
                    target_host,
                    target_port,
                )
                .await?;
                Ok(BoxedStream::new(stream))
            }
            // v1 seam: onion-wrap across `self.hops` and dial hop 1.
            n => anyhow::bail!("multi-hop circuits (n={n}) are a v1 feature; v0 is single-hop"),
        }
    }
}

/// Produces a byte pipe to a target through the overlay. The shim depends on
/// this rather than a concrete `Circuit` so v1 path selection / rebuild can slot
/// in without touching the shim.
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
