//! The relay client: QUIC to a relay, device-signature handshake, `Connect`,
//! then the raw byte pipe the caller runs its own TLS over.

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::CertificateDer;

use crate::proto::{self, Connect, DeviceCertMaterial, MlDsaSigningKey};
use crate::stream::RelayStream;
use crate::tls::{self, PinnedServerCertVerifier, RELAY_SERVER_NAME};

/// The device identity a client authenticates with (design §9.4): the ML-DSA-44
/// signing key plus the offline cert chain the relay verifies. A device with no
/// cert yet cannot build one, so its traffic stays direct.
pub struct ClientIdentity {
    pub user_id: String,
    pub device_id: String,
    /// Presented so the cert payload reconstructs exactly; nothing signs under it.
    pub device_ed25519_pub: [u8; 32],
    pub signing_key: MlDsaSigningKey,
    pub cert: DeviceCertMaterial,
}

impl ClientIdentity {
    pub fn new(
        user_id: impl Into<String>,
        device_id: impl Into<String>,
        device_ed25519_pub: [u8; 32],
        signing_key: MlDsaSigningKey,
        cert: DeviceCertMaterial,
    ) -> Self {
        ClientIdentity {
            user_id: user_id.into(),
            device_id: device_id.into(),
            device_ed25519_pub,
            signing_key,
            cert,
        }
    }
}

impl ClientIdentity {
    /// A freshly-nonced handshake for one hop. Never reuse one across hops: a
    /// neighbour could replay it.
    pub fn fresh_handshake(&self) -> anyhow::Result<proto::Handshake> {
        let mut nonce = [0u8; 32];
        getrandom::getrandom(&mut nonce).map_err(|e| anyhow::anyhow!("nonce rng: {e}"))?;
        Ok(proto::sign_handshake(
            &self.signing_key,
            &self.user_id,
            &self.device_id,
            self.device_ed25519_pub,
            self.cert.clone(),
            proto::now_unix(),
            nonce,
        ))
    }
}

/// An open QUIC connection to one relay, with the negotiated wire generation.
///
/// Versioning is entirely ALPN ([`proto::SUPPORTED_ALPNS`]); no overlap fails
/// the QUIC handshake, never a half-spoken stream.
pub struct RelayLink {
    connection: quinn::Connection,
    endpoint: quinn::Endpoint,
    version: u8,
}

impl RelayLink {
    /// Dial `relay_addr`, pinning `relay_cert` as the relay's identity.
    ///
    /// One QUIC endpoint per call, owned by the stream opened from it.
    /// (TODO Slice 1b: pool one endpoint per relay.)
    pub async fn open(
        relay_addr: SocketAddr,
        relay_cert: &CertificateDer<'static>,
    ) -> anyhow::Result<RelayLink> {
        tls::ensure_crypto_provider();

        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(PinnedServerCertVerifier::new(relay_cert.clone()))
            .with_no_client_auth();
        client_crypto.alpn_protocols =
            proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();

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
        let version = negotiated_version(&connection)?;
        Ok(RelayLink {
            connection,
            endpoint,
            version,
        })
    }

    /// Multi-hop needs [`proto::ONION_MIN_VERSION`].
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Open a bi-stream; the [`RelayStream`] owns the connection and endpoint.
    pub async fn open_stream(self) -> anyhow::Result<RelayStream> {
        let (send, recv) = self.connection.open_bi().await?;
        Ok(RelayStream::new(
            send,
            recv,
            Some(self.connection),
            Some(self.endpoint),
        ))
    }
}

fn negotiated_version(connection: &quinn::Connection) -> anyhow::Result<u8> {
    let alpn = connection
        .handshake_data()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.protocol)
        .ok_or_else(|| anyhow::anyhow!("relay negotiated no ALPN"))?;
    proto::version_from_alpn(&alpn).ok_or_else(|| {
        anyhow::anyhow!(
            "relay negotiated an unsupported protocol: {}",
            String::from_utf8_lossy(&alpn)
        )
    })
}

pub struct RelayClient;

impl RelayClient {
    /// Single-hop: connect, authenticate as `identity`, request
    /// `Connect(host, port)`, and return the live byte pipe.
    pub async fn connect(
        relay_addr: SocketAddr,
        relay_cert: &CertificateDer<'static>,
        identity: &ClientIdentity,
        host: &str,
        port: u16,
    ) -> anyhow::Result<RelayStream> {
        let link = RelayLink::open(relay_addr, relay_cert).await?;
        let version = link.version();
        let mut stream = link.open_stream().await?;

        let handshake = identity.fresh_handshake()?;
        proto::write_handshake(&mut stream, &handshake, version).await?;
        proto::write_connect(
            &mut stream,
            &Connect {
                host: host.to_string(),
                port,
            },
            version,
        )
        .await?;

        proto::read_response(&mut stream).await?;
        Ok(stream)
    }
}
