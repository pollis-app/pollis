//! The relay client: open a QUIC connection to a relay, run the device-signature
//! handshake, request `Connect(host, port)`, and on `Ok` hand back the raw byte
//! pipe (over which the caller runs its own TLS to the real host).

use std::net::SocketAddr;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::CertificateDer;

use crate::proto::{self, Connect};
use crate::stream::RelayStream;
use crate::tls::{self, PinnedServerCertVerifier, RELAY_SERVER_NAME};

/// The device identity a client authenticates with (design §9.4: reuse the DS
/// Ed25519 device key).
pub struct ClientIdentity {
    pub user_id: String,
    pub device_id: String,
    pub signing_key: SigningKey,
}

impl ClientIdentity {
    pub fn new(user_id: impl Into<String>, device_id: impl Into<String>, signing_key: SigningKey) -> Self {
        ClientIdentity {
            user_id: user_id.into(),
            device_id: device_id.into(),
            signing_key,
        }
    }
}

/// A single relay endpoint: its address and the cert the client pins as its
/// identity. Building the client config from this keeps the relay's identity
/// out-of-band (a later slice fetches it from Turso).
pub struct RelayClient;

impl RelayClient {
    /// Connect to `relay_addr` (pinning `relay_cert`), authenticate as
    /// `identity`, request `Connect(host, port)`, and return the live byte pipe.
    ///
    /// A fresh QUIC endpoint is created per call for v0 simplicity; the returned
    /// [`RelayStream`] owns it so it lives as long as the pipe. (TODO Slice 1b:
    /// pool one endpoint per relay.)
    pub async fn connect(
        relay_addr: SocketAddr,
        relay_cert: &CertificateDer<'static>,
        identity: &ClientIdentity,
        host: &str,
        port: u16,
    ) -> anyhow::Result<RelayStream> {
        tls::ensure_crypto_provider();

        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(PinnedServerCertVerifier::new(relay_cert.clone()))
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![proto::ALPN.to_vec()];

        let quic_crypto = QuicClientConfig::try_from(client_crypto)?;
        let client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

        let bind: SocketAddr = if relay_addr.is_ipv6() {
            "[::]:0".parse().unwrap()
        } else {
            "0.0.0.0:0".parse().unwrap()
        };
        let mut endpoint = quinn::Endpoint::client(bind)?;
        endpoint.set_default_client_config(client_config);

        let connection = endpoint.connect(relay_addr, RELAY_SERVER_NAME)?.await?;
        let (mut send, mut recv) = connection.open_bi().await?;

        // Pipeline handshake + connect, then read the single terminal response.
        let mut nonce = [0u8; 32];
        getrandom::getrandom(&mut nonce).map_err(|e| anyhow::anyhow!("nonce rng: {e}"))?;
        let handshake = proto::sign_handshake(
            &identity.signing_key,
            &identity.user_id,
            &identity.device_id,
            proto::now_unix(),
            nonce,
        );
        proto::write_handshake(&mut send, &handshake).await?;
        proto::write_connect(&mut send, &Connect { host: host.to_string(), port }).await?;

        proto::read_response(&mut recv).await?;

        Ok(RelayStream::new(send, recv, Some(connection), Some(endpoint)))
    }
}
