use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use crate::error::{Error, Result};
use crate::state::AppState;

// ── On-disk media cache ───────────────────────────────────────────────────
//
// Decrypted media is materialised under a content-addressed cache so the
// frontend can render directly from a file path (via `convertFileSrc`) instead
// of pumping multi-megabyte byte arrays through the JSON IPC. Keyed by
// `content_hash`, which uniquely identifies the bytes (it's also the seed for
// the AES-GCM key derivation), so there's no invalidation problem — if the
// file exists, the bytes are correct.

/// Hard cap on total cache size before LRU eviction kicks in.
const MEDIA_CACHE_MAX_BYTES: u64 = 500 * 1024 * 1024;

/// Per-file cap. Files larger than this skip the cache entirely — the
/// caller falls back to the byte path for that one render. Bounds the
/// worst-case eviction storm where a single huge file would push every
/// other entry out.
pub const MEDIA_CACHE_MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Set once at app startup from the Tauri shim (`app_data_dir()`).
static MEDIA_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Per-hash locks so concurrent callers for the same content_hash share one
/// download instead of racing to write the same file. The outer mutex guards
/// the map; the inner `Arc<TokioMutex>` is the actual gate.
static IN_FLIGHT: OnceLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();

fn in_flight() -> &'static StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    IN_FLIGHT.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Initialise the on-disk media cache directory. Must be called once during
/// app setup (the Tauri shim plumbs in `app_data_dir().join("media-cache")`).
/// Idempotent: subsequent calls are ignored.
pub fn set_media_cache_dir(path: PathBuf) {
    let _ = MEDIA_CACHE_DIR.set(path);
}

fn media_cache_dir() -> Result<&'static Path> {
    MEDIA_CACHE_DIR
        .get()
        .map(|p| p.as_path())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media cache dir not initialised")))
}

/// Map a MIME type to a file extension. Falls back to `bin`. Kept small —
/// we only need extensions for the media types Pollis actually renders.
fn ext_for_content_type(ct: &str) -> &'static str {
    match ct {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/webm" => "weba",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        _ => "bin",
    }
}

fn cache_file_path(content_hash: &str, content_type: &str) -> Result<PathBuf> {
    let dir = media_cache_dir()?;
    let ext = ext_for_content_type(content_type);
    Ok(dir.join(format!("{content_hash}.{ext}")))
}

/// Stat every file in the cache; if total size exceeds the cap, delete by
/// oldest mtime first until we're under. No in-memory index — directory is
/// small enough that stat'ing it on each insert is fine.
fn enforce_cache_cap(dir: &Path) {
    enforce_cache_cap_to(dir, MEDIA_CACHE_MAX_BYTES);
}

/// Lower-bound variant: shrink the cache to at most `target_bytes` by
/// evicting oldest entries first. Used both by the regular cap-enforcer
/// and by the pre-write headroom check in `get_media_path`.
fn enforce_cache_cap_to(dir: &Path, target_bytes: u64) {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };

    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        // Ignore in-progress writes.
        if path.extension().is_some_and(|e| e == "tmp") {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let size = meta.len();
        total += size;
        files.push((path, size, mtime));
    }

    if total <= target_bytes {
        return;
    }

    // Oldest first.
    files.sort_by_key(|(_, _, mtime)| *mtime);
    for (path, size, _) in files {
        if total <= target_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Public re-evaluation entry point — call from app focus to defend
/// against external file copies / mtime tampering / cap-config changes.
pub fn enforce_cache_cap_now() {
    if let Some(dir) = MEDIA_CACHE_DIR.get() {
        enforce_cache_cap(dir);
    }
}

/// Sum of all cached file sizes. Used to gate downloads against the cap
/// *before* writing new bytes, so the cache never peaks above the cap.
pub fn cache_total_bytes() -> u64 {
    let dir = match MEDIA_CACHE_DIR.get() {
        Some(d) => d,
        None => return 0,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return 0,
    };
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// Wipe every file in the media cache directory. Called on logout so
/// decrypted images and other media don't sit on disk past a session end —
/// the cache itself is plaintext at rest, so it must follow the same
/// lifecycle as the keystore unlock. The directory itself stays so a
/// subsequent re-login doesn't have to re-create it.
pub fn clear_media_cache() {
    let dir = match MEDIA_CACHE_DIR.get() {
        Some(d) => d,
        None => return,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let _ = std::fs::remove_file(&path);
    }
}

// ── Existing commands (avatars, group icons) ───────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadResult {
    pub key: String,
    pub url: String,
}

pub async fn upload_file(
    key: String,
    data: Vec<u8>,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<UploadResult> {
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);
    r2_put(
        &state.config.r2_endpoint,
        &state.config.r2_access_key_id,
        &state.config.r2_secret_access_key,
        &state.config.r2_region,
        &key,
        data,
        &content_type,
    )
    .await?;
    Ok(UploadResult { key, url })
}

pub async fn download_file(
    key: String,
    state: &Arc<AppState>,
) -> Result<Vec<u8>> {
    r2_get(
        &state.config.r2_endpoint,
        &state.config.r2_access_key_id,
        &state.config.r2_secret_access_key,
        &state.config.r2_region,
        &key,
    )
    .await
}

pub(crate) async fn r2_put(
    endpoint: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    key: &str,
    data: Vec<u8>,
    content_type: &str,
) -> Result<()> {
    let url = format!("{}/{}", endpoint.trim_end_matches('/'), key);
    let auth_headers = sigv4_headers("PUT", &url, content_type, &data, access_key, secret_key, region)?;

    let client = reqwest::Client::new();
    let mut req = client.put(&url).header("Content-Type", content_type).body(data);
    for (k, v) in &auth_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let response = req.send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("R2 upload failed: {} — {}", status, body)));
    }
    Ok(())
}

pub(crate) async fn r2_get(
    endpoint: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    key: &str,
) -> Result<Vec<u8>> {
    let url = format!("{}/{}", endpoint.trim_end_matches('/'), key);
    let auth_headers = sigv4_headers("GET", &url, "", &[], access_key, secret_key, region)?;

    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    for (k, v) in &auth_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let response = req.send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("R2 download failed: {} — {}", status, body)));
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
pub async fn upload_media(
    path: String,
    filename: String,
    content_type: String,
    state: &Arc<AppState>,
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
pub async fn download_media(
    r2_key: String,
    content_hash: String,
    state: &Arc<AppState>,
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

/// Return a filesystem path to the decrypted media bytes.
///
/// The frontend converts the path with `convertFileSrc()` and uses the result
/// directly as `<img src>` / `<video src>`. This avoids serialising the raw
/// bytes over the JSON IPC, which dominates render time for image-heavy
/// channels.
///
/// Cached by `content_hash`. If a file already exists for this hash we return
/// it immediately. Otherwise we download + decrypt, write atomically, then
/// return the path. Concurrent calls for the same hash share one download
/// via a per-hash tokio mutex.
pub async fn get_media_path(
    r2_key: String,
    content_hash: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<String> {
    let target = cache_file_path(&content_hash, &content_type)?;

    // Fast path: already cached. Touch mtime so the LRU sees it as fresh.
    if target.exists() {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&target) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return Ok(target.to_string_lossy().into_owned());
    }

    // Acquire a per-hash lock so the second waiter sees the file on disk
    // instead of starting a redundant download.
    let lock = {
        let mut map = in_flight().lock().expect("in-flight map poisoned");
        map.entry(content_hash.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    // Recheck under the lock — another caller may have just finished.
    if target.exists() {
        // Drop our entry from the in-flight map.
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Ok(target.to_string_lossy().into_owned());
    }

    let bytes = download_media(r2_key, content_hash.clone(), state).await?;

    // Per-file cap. Files larger than MEDIA_CACHE_MAX_FILE_BYTES skip the
    // cache entirely so a single huge upload can't push everything else
    // out. The frontend falls back to the byte path on the empty sentinel.
    if bytes.len() as u64 > MEDIA_CACHE_MAX_FILE_BYTES {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Ok(String::new());
    }

    // Atomic write: <hash>.<ext>.tmp → rename → <hash>.<ext>.
    let dir = media_cache_dir()?;
    if let Err(e) = std::fs::create_dir_all(dir) {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("create cache dir: {e}")));
    }

    // Pre-emptive eviction: if the new file would push us over the cap,
    // evict oldest entries down to (cap - new_file_size) *before* writing.
    // Without this the cache temporarily peaks above the cap during a
    // large download.
    let new_size = bytes.len() as u64;
    let total = cache_total_bytes();
    if total.saturating_add(new_size) > MEDIA_CACHE_MAX_BYTES {
        enforce_cache_cap_to(dir, MEDIA_CACHE_MAX_BYTES.saturating_sub(new_size));
    }

    let mut tmp = target.clone();
    tmp.set_extension(format!(
        "{}.tmp",
        target.extension().and_then(|s| s.to_str()).unwrap_or("bin")
    ));
    if let Err(e) = tokio::fs::write(&tmp, &bytes).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("write cache tmp: {e}")));
    }
    if let Err(e) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("rename cache tmp: {e}")));
    }

    // Belt-and-braces post-write enforcement in case the pre-emptive
    // estimate was off (concurrent inserts, mtime races).
    enforce_cache_cap(dir);

    in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
    Ok(target.to_string_lossy().into_owned())
}

// ── Deletion ──────────────────────────────────────────────────────────────

/// Delete an R2 object by key. Best-effort: returns Err on network / auth
/// failures so callers can log and continue. A 404 is treated as success
/// (the object is already gone, which is the desired end state).
pub(crate) async fn delete_r2_object(
    state: &AppState,
    r2_key: &str,
) -> Result<()> {
    r2_delete(
        &state.config.r2_endpoint,
        &state.config.r2_access_key_id,
        &state.config.r2_secret_access_key,
        &state.config.r2_region,
        r2_key,
    )
    .await
}

pub(crate) async fn r2_delete(
    endpoint: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    key: &str,
) -> Result<()> {
    let url = format!("{}/{}", endpoint.trim_end_matches('/'), key);
    let auth_headers = sigv4_headers("DELETE", &url, "", &[], access_key, secret_key, region)?;

    let client = reqwest::Client::new();
    let mut req = client.delete(&url);
    for (k, v) in &auth_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 404 {
        return Ok(());
    }

    let body = resp.text().await.unwrap_or_default();
    Err(Error::Other(anyhow::anyhow!(
        "R2 delete failed: {} — {}", status, body
    )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn creds() -> Option<(String, String, String, String)> {
        let _ = dotenvy::from_filename(".env.development");
        let _ = dotenvy::from_filename("../.env.development");
        let endpoint = std::env::var("R2_S3_ENDPOINT").ok()?;
        let access = std::env::var("R2_ACCESS_KEY_ID").ok()?;
        let secret = std::env::var("R2_SECRET_KEY")
            .or_else(|_| std::env::var("R2_SECRET_ACCESS_KEY"))
            .ok()?;
        let region = std::env::var("R2_REGION").unwrap_or_else(|_| "auto".to_string());
        Some((endpoint, access, secret, region))
    }

    fn test_key(label: &str) -> String {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let pid = std::process::id();
        format!("tests/integration-{pid}-{nanos}/{label}")
    }

    macro_rules! creds_or_skip {
        () => {
            match creds() {
                Some(c) => c,
                None => {
                    eprintln!("R2 creds missing in .env.development — skipping");
                    return;
                }
            }
        };
    }

    #[tokio::test]
    async fn upload_download_roundtrip() {
        let (ep, ak, sk, rg) = creds_or_skip!();
        let key = test_key("roundtrip.bin");
        let payload = b"hello pollis r2 integration test".to_vec();

        r2_put(&ep, &ak, &sk, &rg, &key, payload.clone(), "application/octet-stream")
            .await
            .expect("put");

        let got = r2_get(&ep, &ak, &sk, &rg, &key).await.expect("get");
        assert_eq!(got, payload, "round-trip bytes mismatch");

        let _ = r2_delete(&ep, &ak, &sk, &rg, &key).await;
    }

    #[tokio::test]
    async fn overwrite_at_same_key_returns_new_bytes() {
        let (ep, ak, sk, rg) = creds_or_skip!();
        let key = test_key("overwrite.bin");
        let a = b"AAAA-first-version".to_vec();
        let b = b"BBBB-second-version-with-different-length".to_vec();

        r2_put(&ep, &ak, &sk, &rg, &key, a.clone(), "application/octet-stream")
            .await
            .expect("put A");
        r2_put(&ep, &ak, &sk, &rg, &key, b.clone(), "application/octet-stream")
            .await
            .expect("put B");

        let got = r2_get(&ep, &ak, &sk, &rg, &key).await.expect("get");
        assert_eq!(got, b, "overwrite did not replace bytes");
        assert_ne!(got, a, "overwrite still returned old bytes");

        let _ = r2_delete(&ep, &ak, &sk, &rg, &key).await;
    }

    #[tokio::test]
    async fn delete_removes_object() {
        let (ep, ak, sk, rg) = creds_or_skip!();
        let key = test_key("delete.bin");

        r2_put(&ep, &ak, &sk, &rg, &key, b"delete me".to_vec(), "application/octet-stream")
            .await
            .expect("put");
        r2_delete(&ep, &ak, &sk, &rg, &key).await.expect("delete");

        let result = r2_get(&ep, &ak, &sk, &rg, &key).await;
        assert!(result.is_err(), "GET after DELETE should fail, got Ok");
    }

    #[tokio::test]
    async fn delete_of_missing_key_is_ok() {
        let (ep, ak, sk, rg) = creds_or_skip!();
        let key = test_key("never-existed.bin");

        r2_delete(&ep, &ak, &sk, &rg, &key)
            .await
            .expect("delete of missing key should return Ok (404 == success)");
    }
}
