//! Signing-key loading and dev keygen.
//!
//! Real key custody (HSM / KMS) is a later slice. Here a key comes from either
//! an env var (32-byte hex) or a key file (32-byte hex). If neither is present
//! the build **refuses** rather than inventing a key, so a prod bundle can
//! never be signed with an accidental ephemeral key.
//!
//! The key is ML-DSA-44 since #668. An ML-DSA private key IS its 32-byte seed,
//! so the *secret* custody format did not change at all — only the public half
//! grew (32 → 1312 bytes), and it is published, not stored.

use std::path::Path;

use ml_dsa::{Keypair, MlDsa44, SigningKey};
use rand_core::{OsRng, RngCore as _};

use crate::error::{BuilderError, Result};

/// A freshly generated dev keypair, hex-encoded.
pub struct GeneratedKey {
    /// 32-byte ML-DSA-44 seed (the whole private key), lowercase hex.
    pub secret_hex: String,
    /// 1312-byte ML-DSA-44 public key, lowercase hex.
    pub public_hex: String,
}

/// Parse a 32-byte hex string into a `SigningKey`.
fn parse_hex_key(hex_str: &str) -> Result<SigningKey<MlDsa44>> {
    let trimmed = hex_str.trim();
    let bytes = hex::decode(trimmed)
        .map_err(|e| BuilderError::SigningKey(format!("not valid hex: {e}")))?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        BuilderError::SigningKey(format!("expected 32 bytes, got {}", bytes.len()))
    })?;
    Ok(SigningKey::<MlDsa44>::from_seed(&arr.into()))
}

/// Load the signing key from, in order: the env var `env_var`, then
/// `key_file` if provided. If neither yields a key, return [`BuilderError::NoSigningKey`].
pub fn load_signing_key(env_var: &str, key_file: Option<&Path>) -> Result<SigningKey<MlDsa44>> {
    if let Ok(hex_str) = std::env::var(env_var) {
        if !hex_str.trim().is_empty() {
            return parse_hex_key(&hex_str);
        }
    }
    if let Some(path) = key_file {
        let contents = std::fs::read_to_string(path)?;
        return parse_hex_key(&contents);
    }
    Err(BuilderError::NoSigningKey(env_var.to_string()))
}

/// Mint a throwaway ML-DSA-44 keypair for dev use.
pub fn generate() -> GeneratedKey {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let signing_key = SigningKey::<MlDsa44>::from_seed(&seed.into());
    GeneratedKey {
        secret_hex: hex::encode(seed),
        public_hex: hex::encode(signing_key.verifying_key().encode()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_roundtrips() {
        let g = generate();
        let key = parse_hex_key(&g.secret_hex).unwrap();
        assert_eq!(hex::encode(key.verifying_key().encode()), g.public_hex);
    }

    #[test]
    fn rejects_short_key() {
        assert!(parse_hex_key("00").is_err());
    }
}
