//! The onion layer: one nested TLS 1.3 session per hop (design §6.2, #813).
//!
//! # Why TLS rather than a bespoke onion cipher
//!
//! A relay's identity is its pinned leaf cert, so a layer is a TLS 1.3 session to it over the previous hop.
//!
//! A reviewed key schedule and per-circuit forward secrecy, for one extra round trip per hop.
//!
//! # What each party can see
//!
//! Hop *i* peels one layer; only the last hop sees the `Connect`:
//!
//! - hop 1 learns the client's address and hop 2's address, never the destination;
//! - a middle hop learns only its two neighbours;
//! - the last hop learns the destination and its predecessor, never the client's
//!   address.
//!
//! The client pins each hop's cert inside its layer, so a forwarding relay cannot impersonate the next.
//!
//! Every hop still learns the account (device cert): this is address unlinkability, not anonymity.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::proto::ONION_ALPN;
use crate::tls::{self, PinnedServerCertVerifier, SelfSignedIdentity, RELAY_SERVER_NAME};

/// TLS 1.3 only, explicitly: feature unification could otherwise enable `tls12`.
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

/// Build the acceptor a relay uses to peel a layer addressed to itself; stateless, built once.
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
