//! TLS/crypto plumbing for the outer relay hop.
//!
//! The client pins the relay's exact self-signed cert; the pinned verifier still checks CertificateVerify.
//!
//! The inner TLS is never touched here.

use std::sync::{Arc, Once};

use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// Nominal subject/server name; verification is by pinning.
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

/// Load the persisted QUIC identity (`key_path`, `<key_path>.crt`), generating one if absent.
pub fn load_or_generate_identity(key_path: &str) -> anyhow::Result<SelfSignedIdentity> {
    let cert_path = format!("{key_path}.crt");
    if std::path::Path::new(key_path).exists() && std::path::Path::new(&cert_path).exists() {
        let key_bytes = std::fs::read(key_path)?;
        let cert_bytes = std::fs::read(&cert_path)?;
        let key_der = PrivateKeyDer::try_from(key_bytes)
            .map_err(|e| anyhow::anyhow!("load relay key {key_path}: {e}"))?;
        return Ok(SelfSignedIdentity {
            cert_der: CertificateDer::from(cert_bytes),
            key_der,
        });
    }

    let identity = generate_self_signed(RELAY_SERVER_NAME)?;
    if let Some(parent) = std::path::Path::new(key_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&cert_path, identity.cert_der.as_ref())?;
    std::fs::write(key_path, identity.key_der.secret_der())?;
    restrict_key_perms(key_path);
    Ok(identity)
}

/// Best-effort `0600` on the persisted private key (Unix). A no-op elsewhere.
fn restrict_key_perms(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// A CA and a leaf it issued for a DNS name (tests only).
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

/// Accepts exactly one pinned leaf; signatures are still verified, so the peer must hold its key.
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

/// A `ServerCertVerifier` that accepts exactly one leaf cert **by SHA-256**.
///
/// For a relay dialing an `Extend` target; the client pins the full DER inside the onion layer anyway.
#[derive(Debug)]
pub struct FingerprintPinnedVerifier {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl FingerprintPinnedVerifier {
    pub fn new(fingerprint: [u8; 32]) -> Arc<Self> {
        ensure_crypto_provider();
        Arc::new(Self {
            fingerprint,
            provider: ring_provider(),
        })
    }
}

impl ServerCertVerifier for FingerprintPinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if crate::proto::cert_fingerprint(end_entity.as_ref()) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "next-hop cert does not match the pinned fingerprint".into(),
            ))
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
