//! App key-value state (#2004): sealed on the bot's hosts, stored on the DS,
//! shared by every host of the bot.
//!
//! Both keys are derived from the bot's **account secret**, which every host
//! of the bot holds (the first host generated it; each added host received it
//! wrapped), so any host can read what any other wrote, and the DS — which
//! never sees the account secret — sees only:
//!
//! - `key_id = HMAC-SHA256(mac_key, key)`: the App's key names stay on the hosts;
//! - `sealed = nonce || ChaCha20-Poly1305(seal_key, len(key) || key || value)`,
//!   with the installation id and `key_id` as associated data, so a sealed
//!   value moved to another key or installation does not open;
//! - sizes, versions and write times.
//!
//! Writes go through [`commit_app_state`] under a trigger claim this host won,
//! at the versions it read (optimistic: a write against a moved key is a
//! conflict and nothing lands), once per claim.

use std::sync::Arc;

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use pollis_api::bots::{AppStateCommitBody, AppStateCommitResponse, AppStateReadBody, AppStateWrite};
use rand::RngCore as _;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Bytes a sealed entry adds to its key and value: nonce, tag, key length.
pub const SEAL_OVERHEAD: u64 = 12 + 16 + 2;
const DOMAIN: &[u8] = b"pollis-app-state-v1";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// The two keys an installation's App state is sealed under.
pub struct AppStateKeys {
    installation_id: String,
    mac: Zeroizing<[u8; 32]>,
    seal: Zeroizing<[u8; 32]>,
}

impl AppStateKeys {
    /// Derive from an account secret. Exposed for tests; the runtime uses
    /// [`app_state_keys`].
    pub fn derive(account_secret: &[u8], installation_id: &str) -> AppStateKeys {
        let hk = Hkdf::<Sha256>::new(Some(DOMAIN), account_secret);
        let mut mac = Zeroizing::new([0u8; 32]);
        let mut seal = Zeroizing::new([0u8; 32]);
        let info = |what: &[u8]| [what, b"\0", installation_id.as_bytes()].concat();
        hk.expand(&info(b"key-id"), &mut *mac).expect("32 bytes is a valid HKDF output");
        hk.expand(&info(b"seal"), &mut *seal).expect("32 bytes is a valid HKDF output");
        AppStateKeys {
            installation_id: installation_id.to_string(),
            mac,
            seal,
        }
    }

    /// The opaque id the DS stores `key` under.
    pub fn key_id(&self, key: &str) -> [u8; 32] {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&*self.mac).expect("any key length");
        m.update(key.as_bytes());
        m.finalize().into_bytes().into()
    }

    fn aad(&self, key_id: &[u8; 32]) -> Vec<u8> {
        [DOMAIN, b"\0", self.installation_id.as_bytes(), b"\0", key_id].concat()
    }

    pub fn seal(&self, key: &str, value: &[u8]) -> Result<Vec<u8>> {
        let key_len = u16::try_from(key.len()).map_err(|_| Error::Crypto("App state key too long".into()))?;
        let mut plain = Zeroizing::new(Vec::with_capacity(2 + key.len() + value.len()));
        plain.extend_from_slice(&key_len.to_be_bytes());
        plain.extend_from_slice(key.as_bytes());
        plain.extend_from_slice(value);
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let cipher = ChaCha20Poly1305::new_from_slice(&*self.seal).expect("32-byte key");
        let aad = self.aad(&self.key_id(key));
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &aad })
            .map_err(|_| Error::Crypto("sealing App state".into()))?;
        Ok([&nonce[..], &ct].concat())
    }

    /// Open an entry stored under `key`'s id; fails if it was sealed for
    /// another key, installation or account.
    pub fn open(&self, key: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < SEAL_OVERHEAD as usize {
            return Err(Error::Crypto("App state entry too short".into()));
        }
        let cipher = ChaCha20Poly1305::new_from_slice(&*self.seal).expect("32-byte key");
        let aad = self.aad(&self.key_id(key));
        let plain = Zeroizing::new(
            cipher
                .decrypt(Nonce::from_slice(&sealed[..12]), Payload { msg: &sealed[12..], aad: &aad })
                .map_err(|_| Error::Crypto("App state entry does not open under this bot's key".into()))?,
        );
        let key_len = u16::from_be_bytes([plain[0], plain[1]]) as usize;
        if plain.len() < 2 + key_len || &plain[2..2 + key_len] != key.as_bytes() {
            return Err(Error::Crypto("App state entry names a different key".into()));
        }
        Ok(plain[2 + key_len..].to_vec())
    }
}

/// The keys for the signed-in bot's installation. The installation must be
/// open (`start_unattended`).
pub async fn app_state_keys(state: &Arc<AppState>, installation_id: &str) -> Result<AppStateKeys> {
    let guard = state.unlock.lock().await;
    let unlock = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("bot installation is not active")))?;
    Ok(AppStateKeys::derive(&unlock.account_id_key, installation_id))
}

/// One key as read: its version (0 when absent) and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStateValue {
    pub version: i64,
    pub value: Option<Vec<u8>>,
}

/// Read `key` and the installation's usage in plaintext bytes (keys plus
/// values, the unit of the manifest's `state.quota`).
pub async fn read_app_state(
    state: &Arc<AppState>,
    keys: &AppStateKeys,
    key: &str,
) -> Result<(AppStateValue, u64)> {
    let body = AppStateReadBody {
        installation_id: keys.installation_id.clone(),
        key_ids: vec![b64().encode(keys.key_id(key))],
    };
    let resp = crate::commands::mls::ds_post_json(state, &body).await?;
    let usage = resp.used_bytes.saturating_sub(resp.rows * SEAL_OVERHEAD);
    let value = match resp.entries.into_iter().next() {
        None => AppStateValue { version: 0, value: None },
        Some(e) => {
            let sealed = b64()
                .decode(&e.sealed)
                .map_err(|_| Error::Crypto("App state entry is not base64".into()))?;
            AppStateValue {
                version: e.version,
                value: Some(keys.open(key, &sealed)?),
            }
        }
    };
    Ok((value, usage))
}

/// One write of a job: `key` at the version the job read it at.
pub struct AppStateWriteIn {
    pub key: String,
    pub expected_version: i64,
    pub value: Option<Vec<u8>>,
}

/// Commit a job's writes under the claim this host won on `trigger_key`.
pub async fn commit_app_state(
    state: &Arc<AppState>,
    keys: &AppStateKeys,
    trigger_key: &str,
    writes: &[AppStateWriteIn],
) -> Result<AppStateCommitResponse> {
    let mut wire = Vec::with_capacity(writes.len());
    for w in writes {
        wire.push(AppStateWrite {
            key_id: b64().encode(keys.key_id(&w.key)),
            expected_version: w.expected_version,
            sealed: match &w.value {
                Some(v) => Some(b64().encode(keys.seal(&w.key, v)?)),
                None => None,
            },
        });
    }
    let body = AppStateCommitBody {
        installation_id: keys.installation_id.clone(),
        trigger_key: trigger_key.to_string(),
        writes: wire,
    };
    crate::commands::mls::ds_post_json(state, &body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_host_derives_the_same_keys_and_only_from_the_account_secret() {
        let a = AppStateKeys::derive(b"account-secret", "B1");
        let b = AppStateKeys::derive(b"account-secret", "B1");
        let other_bot = AppStateKeys::derive(b"other-secret", "B1");
        let sealed = a.seal("counter", b"41").unwrap();
        assert_eq!(b.open("counter", &sealed).unwrap(), b"41");
        assert_eq!(a.key_id("counter"), b.key_id("counter"));
        assert_ne!(a.key_id("counter"), other_bot.key_id("counter"));
        assert!(other_bot.open("counter", &sealed).is_err());
    }

    #[test]
    fn a_sealed_value_opens_only_under_its_own_key_and_installation() {
        let keys = AppStateKeys::derive(b"s", "B1");
        let sealed = keys.seal("a", b"v").unwrap();
        assert!(keys.open("b", &sealed).is_err());
        assert!(AppStateKeys::derive(b"s", "B2").open("a", &sealed).is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(keys.open("a", &tampered).is_err());
        assert_eq!(sealed.len() as u64, SEAL_OVERHEAD + 1 + 1);
    }

    #[test]
    fn the_key_name_never_reaches_the_wire_in_the_clear() {
        let keys = AppStateKeys::derive(b"s", "B1");
        let sealed = keys.seal("customer-4711", b"x").unwrap();
        assert!(!sealed.windows(13).any(|w| w == b"customer-4711"));
        assert!(!keys.key_id("customer-4711").windows(8).any(|w| w == b"customer"));
    }
}
