//! TLS/crypto plumbing for the outer relay hop.
//!
//! The relay's QUIC identity is a self-signed cert; the client pins that exact
//! cert (the relay's identity *is* its cert bytes) rather than trusting a CA.
//! This is deliberate: v0 relays are a small known set, and pinning the leaf
//! sidesteps self-signed-as-CA path issues while still binding possession of the
//! private key via the TLS 1.3 CertificateVerify (which the pinned verifier
//! still checks against the ring provider — it does not blindly accept).
//!
//! The INNER TLS (client → real service) is never touched here; the relay only
//! terminates the outer QUIC hop.

use std::sync::{Arc, Once};

use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// Subject name baked into the relay's self-signed cert and used as the QUIC
/// server name on connect. Cert verification is by pinning, so the name is
/// nominal, but a stable value keeps handshakes tidy.
pub const RELAY_SERVER_NAME: &str = "pollis-relay";

static PROVIDER_INIT: Once = Once::new();

/// Install the ring `CryptoProvider` as the process default exactly once. Safe
/// to call from every entry point; the `Once` makes repeat calls no-ops.
pub fn ensure_crypto_provider() {
    PROVIDER_INIT.call_once(|| {
        // Ignore the error: another crate may have already installed a default.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn ring_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A self-signed cert + its private key, DER-encoded.
#[derive(Debug)]
pub struct SelfSignedIdentity {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivateKeyDer<'static>,
}

/// Generate a self-signed identity for `name` (used for the relay's QUIC cert).
pub fn generate_self_signed(name: &str) -> anyhow::Result<SelfSignedIdentity> {
    let mut params = CertificateParams::new(vec![name.to_string()])?;
    params.distinguished_name = DistinguishedName::new();
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok(SelfSignedIdentity {
        cert_der: cert.der().clone(),
        key_der: PrivateKeyDer::try_from(key.serialize_der())
            .map_err(|e| anyhow::anyhow!("serialize key: {e}"))?,
    })
}

/// A CA cert plus a leaf cert issued by it for a given DNS name. Used only by
/// tests to stand up a TLS "origin" server whose cert the client verifies for
/// the *real* name, proving the inner TLS survives tunneling.
#[derive(Debug)]
pub struct IssuedChain {
    pub ca_der: CertificateDer<'static>,
    pub leaf_cert_der: CertificateDer<'static>,
    pub leaf_key_der: PrivateKeyDer<'static>,
}

/// Generate a CA and a leaf cert for `dns_name`, signed by that CA.
pub fn generate_issued_chain(dns_name: &str) -> anyhow::Result<IssuedChain> {
    let mut ca_params = CertificateParams::new(Vec::new())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.distinguished_name = {
        let mut dn = DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "pollis-relay test CA");
        dn
    };
    let ca_key = KeyPair::generate()?;
    let ca_cert = ca_params.self_signed(&ca_key)?;

    let mut leaf_params = CertificateParams::new(vec![dns_name.to_string()])?;
    leaf_params.distinguished_name = DistinguishedName::new();
    let leaf_key = KeyPair::generate()?;
    let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key)?;

    Ok(IssuedChain {
        ca_der: ca_cert.der().clone(),
        leaf_cert_der: leaf_cert.der().clone(),
        leaf_key_der: PrivateKeyDer::try_from(leaf_key.serialize_der())
            .map_err(|e| anyhow::anyhow!("serialize key: {e}"))?,
    })
}

/// A `ServerCertVerifier` that accepts exactly one pinned leaf cert. Signature
/// validation is still delegated to the ring provider, so a peer must actually
/// hold the pinned cert's private key — pinning the bytes is authentication, not
/// a bypass.
#[derive(Debug)]
pub struct PinnedServerCertVerifier {
    pinned: CertificateDer<'static>,
    provider: Arc<CryptoProvider>,
}

impl PinnedServerCertVerifier {
    pub fn new(pinned: CertificateDer<'static>) -> Arc<Self> {
        ensure_crypto_provider();
        Arc::new(Self {
            pinned,
            provider: ring_provider(),
        })
    }
}

impl ServerCertVerifier for PinnedServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.pinned.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("relay cert does not match pin".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
