//! The onion layer: one nested TLS 1.3 session per hop (design §6.2, #813).
//!
//! # Why TLS rather than a bespoke onion cipher
//!
//! A relay's identity in this overlay already *is* its self-signed leaf cert:
//! the directory publishes it, the client pins it, and the outer QUIC hop
//! authenticates possession of the matching private key through the TLS 1.3
//! CertificateVerify (`tls::PinnedServerCertVerifier`). An onion layer needs
//! exactly the same thing — an authenticated, forward-secret channel to a hop
//! whose leaf cert the client already knows — just carried over a byte pipe some
//! *other* relay spliced instead of over a UDP socket. So a layer is a TLS 1.3
//! session to the hop's pinned cert, run over the previous hop's stream.
//!
//! That buys: a reviewed key schedule and AEAD instead of a hand-rolled one,
//! per-circuit forward secrecy (ephemeral TLS 1.3 key exchange), and one
//! authentication story across the whole crate. It costs one extra round trip
//! per hop, which telescoping circuit construction pays anyway.
//!
//! # What each party can see
//!
//! Hop *i* peels exactly one layer. Under it is either another layer (opaque
//! ciphertext, addressed to hop *i+1*) or — at the last hop only — the `Connect`
//! naming the destination. So:
//!
//! - hop 1 learns the client's address and hop 2's address, never the destination;
//! - a middle hop learns only its two neighbours;
//! - the last hop learns the destination and its predecessor, never the client's
//!   address.
//!
//! Authentication is **end to end per hop**: the client pins hop *i*'s leaf cert
//! inside the layer it runs to hop *i*. A forwarding relay therefore cannot
//! substitute itself for the next hop — it would have to present that hop's cert,
//! which it does not hold the key for — and a wrong pin fails the layer handshake
//! outright, before any circuit frame is written.
//!
//! Note what this does **not** hide: every hop still verifies the client's device
//! cert (`proto::verify_handshake`), so every hop learns *which account* is
//! building the circuit, exactly as the single v0 relay does. The property this
//! delivers is network-address unlinkability — no single relay holds both the
//! client's IP and the destination — not anonymity.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::proto::ONION_ALPN;
use crate::tls::{self, PinnedServerCertVerifier, SelfSignedIdentity, RELAY_SERVER_NAME};

/// TLS 1.3 only, both directions. Explicit rather than implied by the crate's
/// rustls feature set: another crate in the workspace may enable `tls12` and
/// feature unification would otherwise silently widen what a layer accepts.
const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Wrap `inner` in an onion layer addressed to the hop whose DER leaf cert is
/// `hop_cert`. The returned stream carries whatever the caller writes to that hop
/// and nothing else; every relay between the caller and that hop sees ciphertext.
///
/// Fails — never falls through in the clear — if the peer at the other end does
/// not present exactly `hop_cert`.
pub async fn client_layer<S>(
    inner: S,
    hop_cert: &CertificateDer<'static>,
) -> anyhow::Result<tokio_rustls::client::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tls::ensure_crypto_provider();

    let mut config = rustls::ClientConfig::builder_with_protocol_versions(TLS13_ONLY)
        .dangerous()
        .with_custom_certificate_verifier(PinnedServerCertVerifier::new(hop_cert.clone()))
        .with_no_client_auth();
    config.alpn_protocols = vec![ONION_ALPN.to_vec()];

    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(RELAY_SERVER_NAME)
        .map_err(|e| anyhow::anyhow!("onion layer server name: {e}"))?;
    let stream = connector
        .connect(server_name, inner)
        .await
        .map_err(|e| anyhow::anyhow!("onion layer handshake to pinned hop failed: {e}"))?;
    Ok(stream)
}

/// Build the acceptor a relay uses to peel a layer addressed to itself. Built
/// once per relay and reused: it is stateless and carries only the node's own
/// QUIC identity, which is the cert clients pin.
pub fn layer_acceptor(identity: &SelfSignedIdentity) -> anyhow::Result<TlsAcceptor> {
    tls::ensure_crypto_provider();

    let mut config = rustls::ServerConfig::builder_with_protocol_versions(TLS13_ONLY)
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.cert_der.clone()],
            identity.key_der.clone_key(),
        )?;
    config.alpn_protocols = vec![ONION_ALPN.to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}
