//! Per-installation webhook secrets, kept in the installation's keystore slot
//! (OS keychain, or the machine-bound encrypted file; never plaintext).
//!
//! Generated on the first start with the webhook enabled and printed once.
//! `pollis-bot rotate-secret` replaces it (and prints the new one once).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine as _;
use rand::RngCore;
use zeroize::Zeroizing;

/// The keystore entry, suffixed with the installation id by `*_for_user`.
pub const WEBHOOK_SECRET_KEY: &str = "pollis_bot_webhook_secret";
/// Every secret starts with this, so secret scanners can recognise one.
pub const SECRET_PREFIX: &str = "pbw_";

#[async_trait]
pub trait SecretSlot: Send + Sync {
    async fn load(&self, installation_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>>;
    async fn store(&self, installation_id: &str, secret: &[u8]) -> Result<()>;
}

#[async_trait]
impl SecretSlot for Arc<dyn pollis_core::keystore::Keystore> {
    async fn load(&self, installation_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self
            .load_for_user(WEBHOOK_SECRET_KEY, installation_id)
            .await?
            .map(Zeroizing::new))
    }

    async fn store(&self, installation_id: &str, secret: &[u8]) -> Result<()> {
        self.store_for_user(WEBHOOK_SECRET_KEY, installation_id, secret).await?;
        Ok(())
    }
}

/// A fresh secret: the prefix and 32 random bytes, base64url.
pub fn generate() -> Zeroizing<Vec<u8>> {
    let mut raw = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(raw.as_mut());
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_ref());
    Zeroizing::new(format!("{SECRET_PREFIX}{encoded}").into_bytes())
}

/// The installation's secret, creating it if absent. The flag is `true` when
/// it was just created, and is the only time the caller should print it.
pub async fn ensure(slot: &dyn SecretSlot, installation_id: &str) -> Result<(Zeroizing<Vec<u8>>, bool)> {
    if let Some(existing) = slot.load(installation_id).await? {
        return Ok((existing, false));
    }
    let secret = generate();
    slot.store(installation_id, &secret).await?;
    Ok((secret, true))
}

/// Replace the installation's secret.
pub async fn rotate(slot: &dyn SecretSlot, installation_id: &str) -> Result<Zeroizing<Vec<u8>>> {
    let secret = generate();
    slot.store(installation_id, &secret).await?;
    Ok(secret)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct MemorySlot(pub Mutex<HashMap<String, Vec<u8>>>);

    #[async_trait]
    impl SecretSlot for MemorySlot {
        async fn load(&self, id: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
            Ok(self.0.lock().unwrap().get(id).cloned().map(Zeroizing::new))
        }
        async fn store(&self, id: &str, secret: &[u8]) -> Result<()> {
            self.0.lock().unwrap().insert(id.into(), secret.to_vec());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_secret_is_created_once_then_reused_and_rotation_replaces_it() {
        let slot = MemorySlot::default();
        let (first, created) = ensure(&slot, "A").await.unwrap();
        assert!(created);
        assert!(first.starts_with(SECRET_PREFIX.as_bytes()));
        let (again, created) = ensure(&slot, "A").await.unwrap();
        assert!(!created);
        assert_eq!(*first, *again);
        let rotated = rotate(&slot, "A").await.unwrap();
        assert_ne!(*first, *rotated);
        let (other, _) = ensure(&slot, "B").await.unwrap();
        assert_ne!(*rotated, *other);
    }
}
