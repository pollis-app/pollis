use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use crate::error::{Error, Result};
use crate::state::AppState;

// ── On-disk media cache ───────────────────────────────────────────────────
//
// Media is materialised on disk **encrypted at rest** under a content-
// addressed cache (`<hash>.<ext>.enc`). The frontend never reads these
// files directly — instead it embeds `http://127.0.0.1:<port>/<token>/<hash>`
// URLs and the loopback media server (`crate::media_server`) decrypts on
// demand. One URL pattern across `<img>/<audio>/<video>` and bytes never
// touch the JSON IPC.
//
// Per-file key derivation: HKDF-SHA256(salt = `pollis-media-cache-v1`,
// ikm = `db_key`, info = content_hash bytes). Different salt from the
// upload-side convergent key (`pollis-att-key`) so the two domains are
// cryptographically separated even though both are seeded from the same
// content hash.

/// Hard cap on total cache size before LRU eviction kicks in.
const MEDIA_CACHE_MAX_BYTES: u64 = 500 * 1024 * 1024;

/// Per-file cap. Files larger than this skip the cache entirely — the
/// caller falls back to the byte path for that one render. Bounds the
/// worst-case eviction storm where a single huge file would push every
/// other entry out.
pub const MEDIA_CACHE_MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Set once at app startup from the Tauri shim (`app_data_dir()`).
static MEDIA_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Per-user scope for the cache. Two clients on the same machine each have
/// their own `db_key`; without per-user scoping they'd share `MEDIA_CACHE_DIR`
/// and try (and fail) to decrypt each other's entries — 500 from the media
/// server. Set after sign-in via `set_cache_user(Some(user_id))`, cleared on
/// logout via `set_cache_user(None)`. The pre-signin window falls back to a
/// shared "_anon" bucket.
static CURRENT_CACHE_USER: StdMutex<Option<String>> = StdMutex::new(None);

pub fn set_cache_user(user_id: Option<&str>) {
    if let Ok(mut guard) = CURRENT_CACHE_USER.lock() {
        *guard = user_id.map(|s| s.to_string());
    }
}

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

fn media_cache_dir() -> Result<PathBuf> {
    let root = MEDIA_CACHE_DIR
        .get()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media cache dir not initialised")))?;
    let user = CURRENT_CACHE_USER
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| "_anon".to_string());
    let path = root.join(user);
    let _ = std::fs::create_dir_all(&path);
    Ok(path)
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
    Ok(dir.join(format!("{content_hash}.{ext}.enc")))
}

/// Map a file extension back to a Content-Type for the HTTP server's
/// response headers. Inverse of `ext_for_content_type`. Mismatches (e.g.
/// the cache was populated under one MIME and the request supplies
/// another) fall back to `application/octet-stream`; the browser
/// usually sniffs anyway.
pub fn content_type_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "weba" => "audio/webm",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

/// Locate the encrypted cache file for a given content hash. Returns the
/// path and the inner extension (between `<hash>.` and `.enc`) so the
/// caller can derive a Content-Type. `None` if no file with this hash
/// exists in the cache.
pub fn find_cached_file(content_hash: &str) -> Option<(PathBuf, String)> {
    let dir = media_cache_dir().ok()?;
    let entries = std::fs::read_dir(&dir).ok()?;
    let prefix = format!("{content_hash}.");
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()).map(str::to_string) {
            Some(n) => n,
            None => continue,
        };
        if !name.starts_with(&prefix) || !name.ends_with(".enc") {
            continue;
        }
        // Strip prefix + trailing `.enc` to get the inner extension.
        let inner = name[prefix.len()..name.len() - ".enc".len()].to_string();
        return Some((path, inner));
    }
    None
}

/// Stat every file in the cache; if total size exceeds the cap, delete by
/// oldest mtime first until we're under. No in-memory index — directory is
/// small enough that stat'ing it on each insert is fine.
fn enforce_cache_cap(dir: &Path) {
    enforce_cache_cap_to(dir, MEDIA_CACHE_MAX_BYTES);
}

/// Lower-bound variant: shrink the cache to at most `target_bytes` by
/// evicting oldest entries first. Used both by the regular cap-enforcer
/// and by the pre-write headroom check in `get_media_url`.
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
    if let Ok(dir) = media_cache_dir() {
        enforce_cache_cap(&dir);
    }
}

/// Sum of all cached file sizes. Used to gate downloads against the cap
/// *before* writing new bytes, so the cache never peaks above the cap.
pub fn cache_total_bytes() -> u64 {
    let dir = match media_cache_dir() {
        Ok(d) => d,
        Err(_) => return 0,
    };
    let entries = match std::fs::read_dir(&dir) {
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
    let dir = match media_cache_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let entries = match std::fs::read_dir(&dir) {
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
    let put_url = presign_r2(state, "put", &key).await?;
    r2_put_url(&put_url, data, &content_type).await?;
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);
    Ok(UploadResult { key, url })
}

pub async fn download_file(
    key: String,
    state: &Arc<AppState>,
) -> Result<Vec<u8>> {
    let get_url = presign_r2(state, "get", &key).await?;
    r2_get_url(&get_url).await
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
        // Encrypt with chunked AES-256-GCM, then upload via a DS-minted
        // presigned PUT (the client holds no R2 credentials).
        let ciphertext = encrypt_chunked(&data, &enc_key, &enc_nonce);

        let put_url = presign_r2(state, "put", &r2_key).await?;
        r2_put_url(&put_url, ciphertext, "application/octet-stream").await?;

        // Register in Turso so future uploads of the same file skip R2 — route the
        // dedup-row write through the Delivery Service.
        let body = serde_json::json!({
            "content_hash": content_hash,
            "r2_key": r2_key,
        });
        crate::commands::mls::ds_post_ok(state, "/v1/attachments/register", &body).await?;
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

    // DS-minted presigned GET — the client holds no R2 credentials. The URL only
    // ever exposes convergently-encrypted ciphertext; confidentiality comes from
    // MLS key distribution, not the R2 ACL (see broker.rs).
    let get_url = presign_r2(state, "get", &r2_key).await?;
    let ciphertext = r2_get_url(&get_url).await?;
    decrypt_chunked(&ciphertext, &enc_key, &enc_nonce)
}

/// Resolve a media attachment to a loopback HTTP URL the webview can use
/// directly as `<img src>` / `<audio src>` / `<video src>`.
///
/// Caches the decrypted-then-cache-encrypted bytes on disk under a
/// content-addressed name so subsequent calls hit the local server
/// without touching R2 again. Bytes never cross the JSON IPC.
///
/// Returns `""` (empty string sentinel) for files larger than
/// `MEDIA_CACHE_MAX_FILE_BYTES`. The frontend falls back to the byte
/// path (`download_media` → in-memory Blob URL) for that one render so
/// a single huge upload can't push everything else out of the cache.
pub async fn get_media_url(
    r2_key: String,
    content_hash: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<String> {
    // Build the URL from the server port + token. Both must be present
    // — without an active unlock the server returns 403 anyway, so
    // there's no point handing out a URL the caller can't use.
    let port = state
        .media_server_port
        .lock()
        .await
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media server not started")))?;
    let token = state
        .media_server_token
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("media server token not set; not unlocked")))?;
    let url = format!("http://127.0.0.1:{port}/{token}/{content_hash}");

    let target = cache_file_path(&content_hash, &content_type)?;

    // Fast path: already cached. Touch mtime so the LRU sees it as fresh.
    if target.exists() {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&target) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return Ok(url);
    }

    // Per-hash lock so the second waiter sees the file on disk instead
    // of starting a redundant download.
    let lock = {
        let mut map = in_flight().lock().expect("in-flight map poisoned");
        map.entry(content_hash.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().await;

    if target.exists() {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Ok(url);
    }

    let bytes = download_media(r2_key, content_hash.clone(), state).await?;

    // Per-file cap. Files larger than MEDIA_CACHE_MAX_FILE_BYTES skip
    // the cache entirely. Empty-string sentinel tells the frontend to
    // fall back to the byte path which produces an in-memory blob URL.
    if bytes.len() as u64 > MEDIA_CACHE_MAX_FILE_BYTES {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Ok(String::new());
    }

    let dir = media_cache_dir()?;
    if let Err(e) = std::fs::create_dir_all(&dir) {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("create cache dir: {e}")));
    }

    // Encrypt before writing. Per-file random nonce + AES-256-GCM under
    // a key derived from `db_key` and the content hash.
    let db_key = {
        let guard = state.unlock.lock().await;
        match guard.as_ref() {
            Some(u) => u.db_key.to_vec(),
            None => {
                in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
                return Err(Error::Other(anyhow::anyhow!(
                    "cannot cache media without an active unlock"
                )));
            }
        }
    };
    let encrypted = match cache_encrypt(&bytes, &db_key, content_hash.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
            return Err(e);
        }
    };

    // Pre-emptive eviction: shrink the cache to (cap - new_file_size)
    // before writing so we never temporarily peak above the cap.
    let new_size = encrypted.len() as u64;
    let total = cache_total_bytes();
    if total.saturating_add(new_size) > MEDIA_CACHE_MAX_BYTES {
        enforce_cache_cap_to(&dir, MEDIA_CACHE_MAX_BYTES.saturating_sub(new_size));
    }

    // Atomic write: <hash>.<ext>.enc.tmp → rename → <hash>.<ext>.enc.
    let mut tmp = target.clone();
    let final_ext = target
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("enc");
    tmp.set_extension(format!("{final_ext}.tmp"));
    if let Err(e) = tokio::fs::write(&tmp, &encrypted).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("write cache tmp: {e}")));
    }
    if let Err(e) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("rename cache tmp: {e}")));
    }

    enforce_cache_cap(&dir);

    in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
    Ok(url)
}

// ── Cache-at-rest crypto ──────────────────────────────────────────────────
//
// Files in `media-cache/` are AES-256-GCM-encrypted under a key derived
// from the active session's `db_key` plus the content hash. Layout:
//
//   [12-byte random nonce][AES-256-GCM(plaintext)][16-byte tag]
//
// Per-file random nonce — the server reads the whole file and decrypts
// in one shot before serving (no streaming AEAD). Total file size is
// bounded by `MEDIA_CACHE_MAX_FILE_BYTES` (100 MiB), well below the
// AES-GCM 64-GiB-per-key safety bound.
//
// Salt domain (`pollis-media-cache-v1`) separates this from the
// upload-side convergent key derivation so a server compromise that
// leaks one key class can't be replayed against the other.

const CACHE_HKDF_SALT: &[u8] = b"pollis-media-cache-v1";
const CACHE_NONCE_LEN: usize = 12;

/// Derive the per-file AES-256-GCM key for cache encryption.
fn derive_cache_key(db_key: &[u8], info: &[u8]) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let hk = Hkdf::<Sha256>::new(Some(CACHE_HKDF_SALT), db_key);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key)
        .expect("HKDF expand for cache key should never fail");
    key
}

/// Encrypt cache bytes. Output layout: `[12-byte nonce][ciphertext+tag]`.
pub fn cache_encrypt(plaintext: &[u8], db_key: &[u8], info: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};
    use rand::RngCore;

    let key = derive_cache_key(db_key, info);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; CACHE_NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| Error::Other(anyhow::anyhow!("media cache encrypt failed")))?;
    let mut out = Vec::with_capacity(CACHE_NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a cache file produced by `cache_encrypt`.
pub fn cache_decrypt(file_bytes: &[u8], db_key: &[u8], info: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};

    if file_bytes.len() < CACHE_NONCE_LEN + 16 {
        return Err(Error::Other(anyhow::anyhow!("media cache file too short")));
    }
    let (nonce_bytes, ct) = file_bytes.split_at(CACHE_NONCE_LEN);
    let key = derive_cache_key(db_key, info);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        .map_err(|_| Error::Other(anyhow::anyhow!("media cache decrypt failed")))
}

// ── Deletion ──────────────────────────────────────────────────────────────

/// Delete an R2 object by key via a DS-minted presigned DELETE. Best-effort:
/// returns Err on network / auth failures so callers can log and continue. A 404
/// is treated as success (the object is already gone, which is the desired end
/// state).
pub(crate) async fn delete_r2_object(
    state: &Arc<AppState>,
    r2_key: &str,
) -> Result<()> {
    let delete_url = presign_r2(state, "delete", r2_key).await?;
    r2_delete_url(&delete_url).await
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

// ── R2 via the DS secrets broker ──────────────────────────────────────────
//
// The client holds NO R2 credentials. Every object access goes through the
// Delivery Service's `/v1/r2/presign` endpoint (device-signed), which returns a
// short-lived SigV4 presigned URL; the client then does a plain, unauthenticated
// HTTP GET/PUT/DELETE against that URL. The presigned URL is self-contained (its
// signature lives in the query string), so no auth headers are attached here.
// The on-device SigV4 signer this replaced held the R2 secret in the client
// bundle — the whole point of the broker is that the secret never ships.
// See `pollis-delivery::broker` and `docs/secrets-broker.md`.

/// Ask the DS to presign an R2 `operation` (`"get"` / `"put"` / `"delete"`) on
/// `key` and return the ready-to-use URL. Device-signed via [`ds_post`].
async fn presign_r2(state: &Arc<AppState>, operation: &str, key: &str) -> Result<String> {
    let body = serde_json::json!({ "operation": operation, "key": key });
    let resp = crate::commands::mls::ds_post(state, "/v1/r2/presign", &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "r2 presign {operation} {status}: {txt}"
        )));
    }
    #[derive(Deserialize)]
    struct PresignResp {
        url: String,
    }
    let parsed: PresignResp = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("r2 presign decode: {e}")))?;
    Ok(parsed.url)
}

/// PUT `data` to a presigned URL. Content-Type is set at request time (the broker
/// signs only `host`, so it is deliberately left unsigned).
async fn r2_put_url(url: &str, data: Vec<u8>, content_type: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .put(url)
        .header("Content-Type", content_type)
        .body(data)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("R2 upload failed: {} — {}", status, body)));
    }
    Ok(())
}

/// GET the bytes at a presigned URL.
async fn r2_get_url(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::Client::new().get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("R2 download failed: {} — {}", status, body)));
    }
    Ok(resp.bytes().await?.to_vec())
}

/// DELETE the object at a presigned URL. A 404 counts as success (already gone).
async fn r2_delete_url(url: &str) -> Result<()> {
    let resp = reqwest::Client::new().delete(url).send().await?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 404 {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    Err(Error::Other(anyhow::anyhow!("R2 delete failed: {} — {}", status, body)))
}
