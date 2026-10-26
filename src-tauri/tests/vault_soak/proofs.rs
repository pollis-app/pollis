//! The checks every hop is put through: plaintext and key-material scanning,
//! entropy, file-format magic, and the AEAD open with right and wrong keys.

use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use aho_corasick::AhoCorasick;
use base64::Engine;
use sha2::{Digest, Sha256};

/// The upload pipeline's AEAD chunk (`commands::r2::CHUNK_SIZE`) and tag.
pub const CHUNK: usize = 4 * 1024 * 1024;
pub const TAG: usize = 16;

/// Ciphertext length the upload pipeline must produce for `n` plaintext bytes.
pub fn sealed_len(n: u64) -> u64 {
    let chunks = n.div_ceil(CHUNK as u64).max(1);
    n + chunks * TAG as u64
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Shannon entropy in bits per byte.
pub fn entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// The entropy a uniformly random buffer of `n` bytes falls below only by
/// chance: the expected shortfall of the empirical entropy is about
/// `255 / (2 n ln 2)` bits, so three times that plus a small margin. `None`
/// below 64 KiB, where the estimate is too noisy to call.
pub fn entropy_floor(n: usize) -> Option<f64> {
    if n < 64 * 1024 {
        return None;
    }
    let shortfall = 255.0 / (2.0 * n as f64 * std::f64::consts::LN_2);
    Some(8.0 - 3.0 * shortfall - 0.002)
}

/// Number of distinct byte values — a window with few is not distinctive
/// enough to be a needle (a run of zeros would "match" anything padded).
pub fn distinct(bytes: &[u8]) -> usize {
    let mut seen = [false; 256];
    for &b in bytes {
        seen[b as usize] = true;
    }
    seen.iter().filter(|s| **s).count()
}

/// A file-format signature at the start of `bytes`, if any.
pub fn known_magic(bytes: &[u8]) -> Option<&'static str> {
    let table: &[(&[u8], usize, &str)] = &[
        (b"\x89PNG\r\n\x1a\n", 0, "PNG"),
        (b"\xff\xd8\xff", 0, "JPEG"),
        (b"GIF8", 0, "GIF"),
        (b"RIFF", 0, "RIFF (WAV/WEBP)"),
        (b"ftyp", 4, "ISO-BMFF (MP4/MOV)"),
        (b"\x1a\x45\xdf\xa3", 0, "Matroska/WebM"),
        (b"%PDF", 0, "PDF"),
        (b"PK\x03\x04", 0, "ZIP"),
        (b"ID3", 0, "MP3"),
        (b"SQLite format 3\0", 0, "SQLite (unencrypted)"),
    ];
    for (sig, at, name) in table {
        if bytes.len() >= at + sig.len() && &bytes[*at..at + sig.len()] == *sig {
            return Some(name);
        }
    }
    None
}

/// What a needle is evidence of when it turns up somewhere it must not.
#[derive(Clone, Debug)]
pub struct Needle {
    /// The corpus file it belongs to (`None` for account-wide secrets).
    #[allow(dead_code)]
    pub file: Option<usize>,
    /// Human-readable description — never the needle's bytes.
    pub what: String,
    pub secret: bool,
}

/// One multi-pattern search over everything that must not appear.
pub struct Scanner {
    ac: Option<AhoCorasick>,
    needles: Vec<Needle>,
    patterns: Vec<Vec<u8>>,
}

impl Scanner {
    pub fn new() -> Self {
        Self { ac: None, needles: Vec::new(), patterns: Vec::new() }
    }

    pub fn add(&mut self, pattern: Vec<u8>, needle: Needle) {
        if pattern.is_empty() {
            return;
        }
        self.patterns.push(pattern);
        self.needles.push(needle);
    }

    /// Add a 32-byte (or other) secret in every encoding it could be stored in.
    pub fn add_secret(&mut self, bytes: &[u8], file: Option<usize>, what: &str) {
        let std = base64::engine::general_purpose::STANDARD;
        let std_np = base64::engine::general_purpose::STANDARD_NO_PAD;
        let url_np = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let forms: Vec<(Vec<u8>, &str)> = vec![
            (bytes.to_vec(), "raw"),
            (hex::encode(bytes).into_bytes(), "hex"),
            (hex::encode_upper(bytes).into_bytes(), "HEX"),
            (std.encode(bytes).into_bytes(), "base64"),
            (std_np.encode(bytes).into_bytes(), "base64-nopad"),
            (url_np.encode(bytes).into_bytes(), "base64url"),
        ];
        for (p, form) in forms {
            self.add(p, Needle { file, what: format!("{what} ({form})"), secret: true });
        }
    }

    pub fn build(mut self) -> Arc<Self> {
        self.ac = Some(AhoCorasick::new(&self.patterns).expect("build needle automaton"));
        Arc::new(self)
    }

    pub fn needles(&self) -> &[Needle] {
        &self.needles
    }

    /// Every distinct needle found in `hay`.
    pub fn scan(&self, hay: &[u8]) -> Vec<usize> {
        let Some(ac) = &self.ac else {
            return Vec::new();
        };
        let mut hits: Vec<usize> = ac
            .find_overlapping_iter(hay)
            .map(|m| m.pattern().as_usize())
            .collect();
        hits.sort_unstable();
        hits.dedup();
        hits
    }

    pub fn describe(&self, hits: &[usize]) -> Vec<String> {
        hits.iter().map(|&i| self.needles[i].what.clone()).collect()
    }

    pub fn clone_unbuilt(&self) -> Scanner {
        Scanner { ac: None, needles: self.needles.clone(), patterns: self.patterns.clone() }
    }
}

/// The verdict on one opaque blob: is it ciphertext-shaped and free of every
/// needle.
#[derive(Clone, Debug, Default)]
pub struct BlobVerdict {
    pub len: usize,
    pub sha256: [u8; 32],
    pub entropy: f64,
    pub entropy_floor: Option<f64>,
    pub hits: Vec<String>,
    pub magic: Option<&'static str>,
}

impl BlobVerdict {
    pub fn of(scanner: &Scanner, bytes: &[u8]) -> Self {
        let hits = scanner.scan(bytes);
        Self {
            len: bytes.len(),
            sha256: sha256(bytes),
            entropy: entropy(bytes),
            entropy_floor: entropy_floor(bytes.len()),
            hits: scanner.describe(&hits),
            magic: known_magic(bytes),
        }
    }

    pub fn high_entropy(&self) -> bool {
        self.entropy_floor.is_none_or(|f| self.entropy >= f)
    }

    pub fn clean(&self) -> bool {
        self.hits.is_empty() && self.magic.is_none() && self.high_entropy()
    }

    pub fn summary(&self) -> String {
        let floor = self
            .entropy_floor
            .map(|f| format!(" (floor {f:.5})"))
            .unwrap_or_else(|| " (too small to call)".to_string());
        format!(
            "{} B, entropy {:.5} bits/B{floor}, needle hits {}, magic {}",
            self.len,
            self.entropy,
            if self.hits.is_empty() { "none".to_string() } else { self.hits.join("; ") },
            self.magic.unwrap_or("none"),
        )
    }
}

/// Open the FIRST chunk of a sealed object (`commands::r2::encrypt_chunked`:
/// chunk 0's nonce is the base nonce unchanged).
pub fn open_chunk0(object: &[u8], key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, ()> {
    let end = object.len().min(CHUNK + TAG);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), &object[..end])
        .map_err(|_| ())
}

/// Open the WHOLE sealed object, chunk by chunk.
pub fn open_all(object: &[u8], key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, ()> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut out = Vec::with_capacity(object.len());
    for (i, chunk) in object.chunks(CHUNK + TAG).enumerate() {
        let mut n = *nonce;
        let idx = (i as u32).to_le_bytes();
        for k in 0..4 {
            n[k] ^= idx[k];
        }
        out.extend_from_slice(&cipher.decrypt(Nonce::from_slice(&n), chunk).map_err(|_| ())?);
    }
    Ok(out)
}

/// The file key `commands::file_keys::derive_file_key` would produce from
/// `ring` — used to model what someone holding only server-side values could
/// derive.
pub fn derive_like_file_key(ring: &[u8; 32], file_id: &[u8; 32]) -> ([u8; 32], [u8; 12]) {
    let hk = hkdf::Hkdf::<Sha256>::new(Some(file_id), ring);
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    hk.expand(b"pollis-file-key-v1", &mut key).expect("hkdf");
    hk.expand(b"pollis-file-nonce-v1", &mut nonce).expect("hkdf");
    (key, nonce)
}

/// The keys an attacker holding everything EXCEPT the account key could try:
/// server-visible values (the file id, the object key, the plaintext hash a
/// convergent scheme would use) fed through the real derivation, and plain
/// random. Every one must fail to open the object.
pub fn attacker_keys(file_id: &[u8; 32], plaintext_sha: &[u8; 32]) -> Vec<(String, [u8; 32], [u8; 12])> {
    let mut out = Vec::new();
    let (k, n) = derive_like_file_key(&[0u8; 32], file_id);
    out.push(("HKDF(file id, zero ring)".to_string(), k, n));
    let (k, n) = derive_like_file_key(file_id, file_id);
    out.push(("HKDF(file id, ring = file id)".to_string(), k, n));
    let (k, n) = derive_like_file_key(plaintext_sha, plaintext_sha);
    out.push(("convergent: HKDF(sha256(plaintext))".to_string(), k, n));
    let mut k = [0u8; 32];
    let mut n = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut k);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut n);
    out.push(("random key".to_string(), k, n));
    out
}
