//! The relay client: open a QUIC connection to a relay, run the device-signature
//! handshake, request `Connect(host, port)`, and on `Ok` hand back the raw byte
//! pipe (over which the caller runs its own TLS to the real host).

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::CertificateDer;

use crate::proto::{self, Connect, DeviceCertMaterial, MlDsaSigningKey};
use crate::stream::RelayStream;
use crate::tls::{self, PinnedServerCertVerifier, RELAY_SERVER_NAME};

/// The device identity a client authenticates with (design §9.4). Carries the
/// device's ML-DSA-44 signing key and its classic Ed25519 leaf public key, AND
/// the offline cert chain the relay verifies: `account_id_pub` + the
/// `device_cert` binding both leaf keys to it (+ `identity_version` /
/// `issued_at`). In `pollis-core` these come from the device's stored cert
/// material (keystore + `user_device`); a device with no cert yet
/// (pre-enrollment/OTP bootstrap) simply cannot build a [`ClientIdentity`], so
/// its traffic stays direct (documented in `docs/relay-operations.md`).
pub struct ClientIdentity {
    pub user_id: String,
    pub device_id: String,
    /// The classic leaf key. Presented so the cert payload reconstructs exactly;
    /// nothing signs under it.
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
    /// Produce a freshly-nonced, freshly-timestamped handshake for one hop. Each
    /// hop of a circuit gets its own — reusing one across hops would hand every
    /// relay a token its neighbour could replay.
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

/// An open QUIC connection to one relay, with the wire generation both ends
/// agreed on.
///
/// Version negotiation lives entirely in ALPN: the client offers every generation
/// it speaks ([`proto::SUPPORTED_ALPNS`], newest first) and the relay picks. A
/// peer with no overlap fails the QUIC handshake outright, so a mismatch is a
/// clean connection error and never a half-spoken stream.
pub struct RelayLink {
    connection: quinn::Connection,
    endpoint: quinn::Endpoint,
    version: u8,
}

impl RelayLink {
    /// Dial `relay_addr`, pinning `relay_cert` as the relay's identity, and
    /// resolve the negotiated wire generation.
    ///
    /// A fresh QUIC endpoint is created per call for v0 simplicity; whatever
    /// stream is opened from it owns it so it lives as long as the pipe.
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

    /// The wire generation this link speaks. Multi-hop needs
    /// [`proto::ONION_MIN_VERSION`].
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Open a bi-stream on this link. The returned [`RelayStream`] owns the QUIC
    /// connection and endpoint, so the link stays alive exactly as long as the
    /// pipe does.
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

/// Read the ALPN the QUIC handshake settled on and map it to a wire generation.
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

/// A single relay endpoint: its address and the cert the client pins as its
/// identity. Building the client config from this keeps the relay's identity
/// out-of-band (a later slice fetches it from Turso).
pub struct RelayClient;

impl RelayClient {
    /// Connect to `relay_addr` (pinning `relay_cert`), authenticate as
    /// `identity`, request `Connect(host, port)`, and return the live byte pipe.
    ///
    /// This is the single-hop path and speaks whichever generation the relay
    /// negotiated — against the shipped v3 fleet it emits byte-for-byte the v3
    /// frames it always did.
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

        // Pipeline handshake + connect, then read the single terminal response.
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
