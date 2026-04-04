use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

// ── Existing commands (avatars, group icons) ───────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadResult {
    pub key: String,
    pub url: String,
}

#[tauri::command]
pub async fn upload_file(
    key: String,
    data: Vec<u8>,
    content_type: String,
    state: State<'_, Arc<AppState>>,
) -> Result<UploadResult> {
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);

    let auth_headers = sigv4_headers(
        "PUT",
        &url,
        &content_type,
        &data,
        &state.config.r2_access_key_id,
        &state.config.r2_secret_access_key,
        &state.config.r2_region,
    )?;

    let client = reqwest::Client::new();
    let mut req = client
        .put(&url)
        .header("Content-Type", &content_type)
        .body(data);

    for (k, v) in &auth_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let response = req.send().await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "R2 upload failed: {} — {}", status, body
        )));
    }

    Ok(UploadResult { key, url })
}

#[tauri::command]
pub async fn download_file(
    key: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<u8>> {
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);

    let auth_headers = sigv4_headers(
        "GET",
        &url,
        "",
        &[],
        &state.config.r2_access_key_id,
        &state.config.r2_secret_access_key,
        &state.config.r2_region,
    )?;

    let client = reqwest::Client::new();
    let mut req = client.get(&url);

    for (k, v) in &auth_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let response = req.send().await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "R2 download failed: {} — {}", status, body
        )));
    }

    Ok(response.bytes().await?.to_vec())
}

// ── Media upload (convergent encryption + cross-user dedup) ───────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaUploadResult {
    pub key: String,
    pub url: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: usize,
    pub content_hash: String,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Upload a media file using convergent encryption.
///
/// Reads the file from the filesystem path (no bytes over IPC), so arbitrarily
/// large files work without memory or serialisation overhead.
///
/// Convergent encryption: SHA-256(plaintext) → deterministic AES-256-GCM key
/// via HKDF.  Same file uploaded by any user produces identical ciphertext →
/// identical R2 object → cross-user deduplication.
///
/// Dedup check against Turso's `attachment_object` table before uploading, so
/// the second upload of the same file by any user skips the R2 PUT entirely.
#[tauri::command]
pub async fn upload_media(
    path: String,
    filename: String,
    content_type: String,
    state: State<'_, Arc<AppState>>,
) -> Result<MediaUploadResult> {
    // Read plaintext from disk.
    let data = tokio::fs::read(&path).await
        .map_err(|e| Error::Other(anyhow::anyhow!("read file {path}: {e}")))?;

    let size_bytes = data.len();

    // SHA-256 of plaintext — the dedup + key-derivation anchor.
    let hash_bytes = sha256_bytes(&data);
    let content_hash = hex::encode(hash_bytes);

    // Deterministic R2 key: same content → same path in R2.
    // Sanitise the filename so the URL path only contains chars that are safe
    // in both URLs and S3 keys without percent-encoding.  The content_hash is
    // the actual uniqueness anchor, so the filename here is decorative.
    let r2_key = format!("media/{}/{}.enc", content_hash, sanitize_key_segment(&filename));
    let r2_url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), r2_key);

    // Derive encryption key and nonce from the content hash (convergent).
    let (enc_key, enc_nonce) = derive_attachment_key(&hash_bytes);

    // Compute blurhash + dimensions before data is consumed.
    let (blurhash, width, height) = if content_type.starts_with("image/") {
        match compute_image_meta(&data) {
            Ok((bh, w, h)) => (Some(bh), Some(w), Some(h)),
            Err(e) => {
                eprintln!("[upload_media] image meta failed for {filename}: {e}");
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    // Check Turso for an existing object with the same content hash.
    let already_uploaded = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT 1 FROM attachment_object WHERE content_hash = ?1",
            libsql::params![content_hash.clone()],
        ).await?;
        rows.next().await?.is_some()
    };

    if !already_uploaded {
        // Encrypt with chunked AES-256-GCM, then upload.
        let ciphertext = encrypt_chunked(&data, &enc_key, &enc_nonce);

        let auth_headers = sigv4_headers(
            "PUT",
            &r2_url,
            "application/octet-stream",
            &ciphertext,
            &state.config.r2_access_key_id,
            &state.config.r2_secret_access_key,
            &state.config.r2_region,
        )?;

        let client = reqwest::Client::new();
        let mut req = client
            .put(&r2_url)
            .header("Content-Type", "application/octet-stream")
            .body(ciphertext);
        for (k, v) in &auth_headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Other(anyhow::anyhow!(
                "R2 upload failed: {} — {}", status, body
            )));
        }

        // Register in Turso so future uploads of the same file skip R2.
        let conn = state.remote_db.conn().await?;
        conn.execute(
            "INSERT OR IGNORE INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
            libsql::params![content_hash.clone(), r2_key.clone()],
        ).await?;
    }

    Ok(MediaUploadResult {
        key: r2_key,
        url: r2_url,
        filename,
        content_type,
        size_bytes,
        content_hash,
        blurhash,
        width,
        height,
    })
}

// ── Media download (decrypt on the way out) ───────────────────────────────

/// Download and decrypt a media attachment.
///
/// The content_hash is embedded in the MLS-encrypted message content, so only
/// group members who can decrypt the message can derive the decryption key.
#[tauri::command]
pub async fn download_media(
    r2_key: String,
    content_hash: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<u8>> {
    let hash_bytes = hex::decode(&content_hash)
        .map_err(|e| Error::Other(anyhow::anyhow!("invalid content_hash: {e}")))?;
    let hash_array: [u8; 32] = hash_bytes.try_into()
        .map_err(|_| Error::Other(anyhow::anyhow!("content_hash must be 32 hex bytes")))?;

    let (enc_key, enc_nonce) = derive_attachment_key(&hash_array);

    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), r2_key);
    let auth_headers = sigv4_headers(
        "GET",
        &url,
        "",
        &[],
        &state.config.r2_access_key_id,
        &state.config.r2_secret_access_key,
        &state.config.r2_region,
    )?;

    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    for (k, v) in &auth_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "R2 download failed: {} — {}", status, body
        )));
    }

    let ciphertext = resp.bytes().await?.to_vec();
    decrypt_chunked(&ciphertext, &enc_key, &enc_nonce)
}

// ── Crypto helpers ────────────────────────────────────────────────────────

/// Chunk size for AES-256-GCM encryption. Each chunk is encrypted independently
/// so arbitrarily large files can be processed without buffering everything.
const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// AES-256-GCM ciphertext overhead per chunk (authentication tag).
const TAG_SIZE: usize = 16;

/// Derive a deterministic AES-256-GCM key and base nonce from the content hash
/// using HKDF-SHA256. Convergent: same hash → same key → same ciphertext.
fn derive_attachment_key(content_hash: &[u8; 32]) -> ([u8; 32], [u8; 12]) {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let hk = Hkdf::<Sha256>::new(None, content_hash);
    let mut key = [0u8; 32];
    hk.expand(b"pollis-att-key", &mut key)
        .expect("HKDF expand for key should never fail");
    let mut nonce = [0u8; 12];
    hk.expand(b"pollis-att-nonce", &mut nonce)
        .expect("HKDF expand for nonce should never fail");
    (key, nonce)
}

/// Encrypt plaintext with chunked AES-256-GCM.
/// Per-chunk nonce = base_nonce XOR little-endian chunk index (first 4 bytes).
/// Output: flat concatenation of encrypted chunks (each = plaintext_chunk + 16-byte tag).
fn encrypt_chunked(data: &[u8], key: &[u8; 32], base_nonce: &[u8; 12]) -> Vec<u8> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    // Allocate enough: plaintext + one 16-byte tag per chunk.
    let n_chunks = data.len().div_ceil(CHUNK_SIZE);
    let mut out = Vec::with_capacity(data.len() + n_chunks * TAG_SIZE);

    for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
        let mut nonce_bytes = *base_nonce;
        let idx = (i as u32).to_le_bytes();
        nonce_bytes[0] ^= idx[0];
        nonce_bytes[1] ^= idx[1];
        nonce_bytes[2] ^= idx[2];
        nonce_bytes[3] ^= idx[3];
        let ct = cipher.encrypt(Nonce::from_slice(&nonce_bytes), chunk)
            .expect("AES-GCM encrypt should not fail");
        out.extend_from_slice(&ct);
    }

    out
}

/// Decrypt ciphertext produced by `encrypt_chunked`.
fn decrypt_chunked(ciphertext: &[u8], key: &[u8; 32], base_nonce: &[u8; 12]) -> Result<Vec<u8>> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let chunk_ct_size = CHUNK_SIZE + TAG_SIZE;
    let mut out = Vec::with_capacity(ciphertext.len());

    for (i, chunk_ct) in ciphertext.chunks(chunk_ct_size).enumerate() {
        let mut nonce_bytes = *base_nonce;
        let idx = (i as u32).to_le_bytes();
        nonce_bytes[0] ^= idx[0];
        nonce_bytes[1] ^= idx[1];
        nonce_bytes[2] ^= idx[2];
        nonce_bytes[3] ^= idx[3];
        let pt = cipher.decrypt(Nonce::from_slice(&nonce_bytes), chunk_ct)
            .map_err(|_| Error::Other(anyhow::anyhow!("attachment decryption failed (chunk {i})")))?;
        out.extend_from_slice(&pt);
    }

    Ok(out)
}

/// Keep only characters that are safe in a URL path segment without encoding.
/// Replaces anything outside [A-Za-z0-9._-] with `_`.
fn sanitize_key_segment(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    use sha2::{Sha256, Digest};
    Sha256::digest(data).into()
}

fn compute_image_meta(data: &[u8]) -> anyhow::Result<(String, u32, u32)> {
    use image::GenericImageView;
    let img = image::load_from_memory(data)
        .map_err(|e| anyhow::anyhow!("image decode: {e}"))?;
    let (width, height) = img.dimensions();
    let rgba = img.to_rgba8();
    let hash = blurhash::encode(4, 3, width, height, rgba.as_raw())
        .map_err(|e| anyhow::anyhow!("blurhash: {e:?}"))?;
    Ok((hash, width, height))
}

// ── SigV4 ─────────────────────────────────────────────────────────────────

/// Compute AWS SigV4 headers for a request.
fn sigv4_headers(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
    access_key: &str,
    secret_key: &str,
    region: &str,
) -> Result<Vec<(String, String)>> {
    use chrono::Utc;

    let (host, path) = parse_host_path(url);

    let now = Utc::now();
    let datetime = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = &datetime[..8];

    let payload_hash = sha256_hex(body);

    let canonical_headers = if content_type.is_empty() {
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{datetime}\n")
    } else {
        format!("content-type:{content_type}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{datetime}\n")
    };

    let signed_headers = if content_type.is_empty() {
        "host;x-amz-content-sha256;x-amz-date"
    } else {
        "content-type;host;x-amz-content-sha256;x-amz-date"
    };

    let canonical_request = format!(
        "{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(secret_key, date, region, "s3");
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let headers = vec![
        ("Authorization".to_string(), authorization),
        ("x-amz-date".to_string(), datetime),
        ("x-amz-content-sha256".to_string(), payload_hash),
    ];

    Ok(headers)
}

fn parse_host_path(url: &str) -> (&str, &str) {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], &without_scheme[i..]),
        None => (without_scheme, "/"),
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Sha256, Digest};
    hex::encode(Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_secret = format!("AWS4{secret}");
    let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}
