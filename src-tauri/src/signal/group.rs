/// Sender Key Distribution for group messaging
/// Each sender maintains a chain key; distributes to group members via X3DH sessions.

use std::collections::HashMap;
use hmac::Hmac;
use hmac::Mac as HmacMac;
use sha2::Sha256;
use aes_gcm::{Aes256Gcm, Key, Nonce, KeyInit};
use aes_gcm::aead::Aead;
use serde::{Deserialize, Serialize};
use crate::error::{Error, Result};

const SENDER_CHAIN_SEED: &[u8] = b"Pollis SenderChain v1";
const SENDER_MSG_SEED: &[u8] = b"Pollis SenderMsg v1";
const MAX_SKIP: u32 = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderKeyState {
    pub chain_id: Vec<u8>,
    pub iteration: u32,
    pub chain_key: Vec<u8>,
    /// Message keys for iterations that were skipped during out-of-order
    /// decryption. Keyed by iteration number.
    #[serde(default)]
    pub skipped_keys: HashMap<u32, Vec<u8>>,
}

impl SenderKeyState {
    pub fn new() -> Self {
        let chain_id: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let chain_key: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        Self {
            chain_id,
            iteration: 0,
            chain_key,
            skipped_keys: HashMap::new(),
        }
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<SenderKeyMessage> {
        let (new_chain_key, message_key) = advance_chain(&self.chain_key)?;
        self.chain_key = new_chain_key;

        let ciphertext = aes_gcm_encrypt(&message_key, plaintext)?;

        let msg = SenderKeyMessage {
            chain_id: self.chain_id.clone(),
            iteration: self.iteration,
            ciphertext,
        };

        self.iteration += 1;
        Ok(msg)
    }

    pub fn decrypt(&mut self, msg: &SenderKeyMessage) -> Result<Vec<u8>> {
        if msg.chain_id != self.chain_id {
            return Err(Error::Signal("sender key chain_id mismatch".into()));
        }

        // Check if we already have a stored key for this iteration
        // (it was skipped during a previous out-of-order decryption).
        if let Some(mk) = self.skipped_keys.remove(&msg.iteration) {
            return aes_gcm_decrypt(&mk, &msg.ciphertext);
        }

        let current = self.iteration;
        if msg.iteration < current {
            return Err(Error::Signal("message iteration already passed".into()));
        }
        if msg.iteration - current > MAX_SKIP {
            return Err(Error::Signal("too many skipped sender key messages".into()));
        }

        // Advance chain, storing message keys for skipped iterations
        let mut ck = self.chain_key.clone();
        for i in current..msg.iteration {
            let (new_ck, mk) = advance_chain(&ck)?;
            self.skipped_keys.insert(i, mk);
            ck = new_ck;
        }

        let (new_ck, message_key) = advance_chain(&ck)?;
        self.chain_key = new_ck;
        self.iteration = msg.iteration + 1;

        aes_gcm_decrypt(&message_key, &msg.ciphertext)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderKeyMessage {
    pub chain_id: Vec<u8>,
    pub iteration: u32,
    pub ciphertext: Vec<u8>,
}

fn advance_chain(chain_key: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut mac_ck = <Hmac<Sha256> as HmacMac>::new_from_slice(chain_key)
        .map_err(|_| Error::Crypto("HMAC init failed".into()))?;
    HmacMac::update(&mut mac_ck, SENDER_CHAIN_SEED);
    let new_ck = HmacMac::finalize(mac_ck).into_bytes().to_vec();

    let mut mac_mk = <Hmac<Sha256> as HmacMac>::new_from_slice(chain_key)
        .map_err(|_| Error::Crypto("HMAC init failed".into()))?;
    HmacMac::update(&mut mac_mk, SENDER_MSG_SEED);
    let mk = HmacMac::finalize(mac_mk).into_bytes().to_vec();

    Ok((new_ck, mk))
}

fn aes_gcm_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let key: [u8; 32] = key[..32].try_into()
        .map_err(|_| Error::Crypto("invalid key size".into()))?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));

    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ct = cipher.encrypt(nonce, plaintext)
        .map_err(|_| Error::Crypto("AES-GCM encrypt failed".into()))?;

    let mut result = nonce_bytes.to_vec();
    result.extend(ct);
    Ok(result)
}

fn aes_gcm_decrypt(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if ciphertext.len() < 12 {
        return Err(Error::Crypto("ciphertext too short".into()));
    }

    let key: [u8; 32] = key[..32].try_into()
        .map_err(|_| Error::Crypto("invalid key size".into()))?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));

    let nonce = Nonce::from_slice(&ciphertext[..12]);
    let ct = &ciphertext[12..];

    cipher.decrypt(nonce, ct)
        .map_err(|_| Error::Crypto("AES-GCM decrypt failed".into()))
}
