use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use crate::error::{Error, Result};
use crate::state::AppState;

// On-disk media cache: content-addressed files (`<hash>.<ext>.enc`) encrypted at
// rest, served decrypted by the loopback media server (`crate::media_server`),
// so bytes never cross the JSON IPC. Per-file key: HKDF-SHA256(salt
// `pollis-media-cache-v1`, ikm `db_key`, info = file id), unrelated to the
// upload-side file key.

/// Hard cap on total cache size before LRU eviction kicks in.
const MEDIA_CACHE_MAX_BYTES: u64 = 500 * 1024 * 1024;

/// Per-file cap. Larger files skip the cache (the caller uses the byte path), so
/// one huge file can't evict everything else.
pub const MEDIA_CACHE_MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Hard ceiling on what any single R2 download may buffer, enforced mid-transfer
/// by [`r2_get_url`].
///
/// A memory-safety bound, not a product limit: downloads are buffered whole, so
/// without it a compromised DS or a wrong R2 object could stream until the
/// process dies. Far above anything this client can upload.
pub const R2_MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Set once at startup from the Tauri shim.
static MEDIA_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The private temp dir holding the test cache root, for the life of the process.
/// A `TempDir` so a test handing the wipe `".."` or a sibling path can only reach
/// into a directory this process owns, even with the guards broken.
#[cfg(test)]
static TEST_CACHE_TEMP: OnceLock<tempfile::TempDir> = OnceLock::new();

#[cfg(test)]
pub(crate) fn test_cache_temp() -> &'static Path {
    TEST_CACHE_TEMP
        .get_or_init(|| tempfile::tempdir().expect("create private temp dir for the cache root"))
        .path()
}

/// The one cache root every test shares ([`MEDIA_CACHE_DIR`] is a `OnceLock`);
/// tests scope themselves by user directory.
#[cfg(test)]
pub(crate) fn test_cache_root() -> &'static Path {
    let root = MEDIA_CACHE_DIR.get_or_init(|| test_cache_temp().join("media-cache"));
    let _ = crate::private_fs::create_dir_all(root);
    root
}

/// Per-user cache scope: each user has their own `db_key`, so a shared directory
/// would fail to decrypt the other's entries. Set after sign-in, cleared on
/// logout; `_anon` before sign-in.
static CURRENT_CACHE_USER: StdMutex<Option<String>> = StdMutex::new(None);

pub fn set_cache_user(user_id: Option<&str>) {
    if let Ok(mut guard) = CURRENT_CACHE_USER.lock() {
        *guard = user_id.map(|s| s.to_string());
    }
}

/// Per-hash locks so concurrent callers for one hash share one download.
static IN_FLIGHT: OnceLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();

fn in_flight() -> &'static StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    IN_FLIGHT.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Initialise the media cache directory, once, at app setup. Later calls are
/// ignored.
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
    let path = user_cache_dir(root, &user).ok_or_else(|| {
        Error::Other(anyhow::anyhow!("media cache user is not a valid id; refusing to scope the cache"))
    })?;
    let _ = crate::private_fs::create_dir_all(&path);
    Ok(path)
}

/// `<root>/<user>`, the only way a user id becomes a cache directory.
///
/// The id originates from the DS, and `join` with an absolute path or `..` escapes
/// the root, so anything not a single safe path component yields `None` ("no
/// cache directory") and can never become a path for the wipe or cap sweep.
fn user_cache_dir(root: &Path, user: &str) -> Option<PathBuf> {
    if !crate::util::is_safe_id(user) {
        return None;
    }
    let path = root.join(user);
    if path.parent() != Some(root) {
        return None;
    }
    Some(path)
}

/// Where `dir` sits relative to the cache root after canonicalizing both. A
/// missing `dir` holds nothing, so `Outside` is right for it too.
#[derive(Debug, PartialEq, Eq)]
enum CacheRootRelation {
    Root,
    Inside,
    /// Anything else, including "cannot tell".
    Outside,
}

fn relation_to_cache_root(root: &Path, dir: &Path) -> CacheRootRelation {
    let (Ok(root), Ok(dir)) = (root.canonicalize(), dir.canonicalize()) else {
        return CacheRootRelation::Outside;
    };
    if dir == root {
        CacheRootRelation::Root
    } else if dir.starts_with(&root) {
        CacheRootRelation::Inside
    } else {
        CacheRootRelation::Outside
    }
}

/// MIME type to file extension for the types Pollis renders; `bin` otherwise.
pub(crate) fn ext_for_content_type(ct: &str) -> &'static str {
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

/// Also used by `commands::emoji`, so emoji land in the same cache and the media
/// server serves them unchanged.
pub(crate) fn cache_file_path(content_hash: &str, content_type: &str) -> Result<PathBuf> {
    let dir = media_cache_dir()?;
    let ext = ext_for_content_type(content_type);
    Ok(dir.join(format!("{content_hash}.{ext}.enc")))
}

/// Extension back to Content-Type for the media server; unknown is
/// `application/octet-stream`.
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

/// The encrypted cache file for `content_hash` and its inner extension, if any.
pub fn find_cached_file(content_hash: &str) -> Option<(PathBuf, String)> {
    let dir = media_cache_dir().ok()?;
    find_cached_file_in(&dir, content_hash)
}

/// [`find_cached_file`] for a named user rather than the ambient one (#1000). The
/// export (#856) must never read another user's directory or `_anon`.
pub(crate) fn find_cached_file_for_user(user_id: &str, content_hash: &str) -> Option<(PathBuf, String)> {
    let root = MEDIA_CACHE_DIR.get()?;
    find_cached_file_in(&user_cache_dir(root, user_id)?, content_hash)
}

fn find_cached_file_in(dir: &Path, content_hash: &str) -> Option<(PathBuf, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
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
        let inner = name[prefix.len()..name.len() - ".enc".len()].to_string();
        return Some((path, inner));
    }
    None
}

/// Remove `user_id`'s cached copy of a file whose last reference they just
/// deleted (#1491). Only a 64-hex hash is accepted, so a malformed reference
/// can't become a pattern matching another entry.
pub(crate) fn evict_cached_file_for_user(user_id: &str, content_hash: &str) {
    if content_hash.len() != 64 || !content_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return;
    }
    let Some(root) = MEDIA_CACHE_DIR.get() else {
        return;
    };
    let Some(dir) = user_cache_dir(root, user_id) else {
        return;
    };
    while let Some((path, _)) = find_cached_file_in(&dir, content_hash) {
        if std::fs::remove_file(&path).is_err() {
            return;
        }
    }
}

/// Counts full walks of the media-cache directory (test-only), so "this path
/// doesn't stat the whole cache" is asserted as a number (#930).
#[cfg(any(test, feature = "test-harness"))]
static CACHE_DIR_WALKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times the media-cache directory has been walked in this process.
#[cfg(any(test, feature = "test-harness"))]
pub fn cache_dir_walks() -> u64 {
    CACHE_DIR_WALKS.load(std::sync::atomic::Ordering::Relaxed)
}

#[inline]
fn note_cache_dir_walk() {
    #[cfg(any(test, feature = "test-harness"))]
    CACHE_DIR_WALKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Evict oldest-mtime files until the cache is under its cap.
///
/// Called by every path that adds bytes to the cache, never by the UI (#930): a
/// cache nobody writes to can't grow, and a sweep on every window focus scaled
/// with the cache size.
fn enforce_cache_cap(dir: &Path) {
    enforce_cache_cap_to(dir, MEDIA_CACHE_MAX_BYTES);
}

/// Shrink the cache to at most `target_bytes`, oldest first. Also the pre-write
/// headroom check in `get_media_url`.
fn enforce_cache_cap_to(dir: &Path, target_bytes: u64) {
    note_cache_dir_walk();
    // Eviction deletes files: only a per-user directory under the root is a valid
    // target.
    let Some(root) = MEDIA_CACHE_DIR.get() else {
        return;
    };
    if relation_to_cache_root(root, dir) != CacheRootRelation::Inside {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };

    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
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

// No public `enforce_cache_cap_now()` on purpose (#930): the cap belongs to the
// write path, not to focus events.

/// Total cached bytes, so downloads are gated against the cap before writing.
pub fn cache_total_bytes() -> u64 {
    note_cache_dir_walk();
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

/// Which slice of the media cache a wipe covers.
///
/// A parameter on purpose (#1000): reading the ambient [`CURRENT_CACHE_USER`] made
/// the target depend on call order, and every caller ended up wiping the empty
/// `_anon` directory instead of the user's.
#[derive(Clone, Copy, Debug)]
pub enum CacheScope<'a> {
    /// One user's cache directory (logout, both unlock paths); other users on the
    /// machine keep theirs.
    User(&'a str),
    /// The whole root, every user's directory and `_anon`: "wipe this computer", and
    /// logout when the accounts index can't say who was signed in.
    Everything,
}

/// Wipe the media cache so decrypted media doesn't outlive its session. The root
/// stays. Resolves the root directly, since `media_cache_dir()` appends the
/// ambient user.
pub fn clear_media_cache(scope: CacheScope<'_>) {
    note_cache_dir_walk();
    let root = match MEDIA_CACHE_DIR.get() {
        Some(r) => r,
        // No cache root was installed (TUI, headless, tests).
        None => return,
    };
    match scope {
        CacheScope::User(user_id) => {
            // An id that isn't one plain path component names no directory; joining it
            // anyway (an absolute `"/home/alice"` replaces the root) is how a DS-chosen id
            // used to become the target.
            if let Some(dir) = user_cache_dir(root, user_id) {
                remove_dir_contents(root, &dir);
            }
        }
        CacheScope::Everything => remove_dir_contents(root, root),
    }
}

/// Empty a directory, keeping it. Refuses any `dir` not resolving to the root or
/// under it: `remove_dir_all` is the most destructive call in the client and
/// `dir` derives from server-chosen data, so the check lives at the deletion.
fn remove_dir_contents(root: &Path, dir: &Path) {
    if relation_to_cache_root(root, dir) == CacheRootRelation::Outside {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Under `Everything` the entries are per-user directories.
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

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
    // Every PUT declares its exact length; the DS signs it into the URL, so R2
    // refuses any other size.
    let put_url =
        presign_r2_with_length(state, "put", &key, Some(data.len() as u64)).await?;
    let overlay = state.overlay_handle();
    r2_put_url(overlay.as_deref(), &put_url, data, &content_type).await?;
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);
    Ok(UploadResult { key, url })
}

pub async fn download_file(
    key: String,
    state: &Arc<AppState>,
) -> Result<Vec<u8>> {
    let get_url = presign_r2(state, "get", &key).await?;
    let overlay = state.overlay_handle();
    // The owned `Vec` conversion happens at this public boundary (#915).
    match r2_get_url(overlay.as_deref(), &get_url, MEDIA_CACHE_MAX_FILE_BYTES).await? {
        Some(bytes) => Ok(bytes.to_vec()),
        None => Err(Error::Other(anyhow::anyhow!(
            "object {key} is over the {MEDIA_CACHE_MAX_FILE_BYTES}-byte download cap"
        ))),
    }
}

// Avatars and group icons are public decoration, stored in the clear under a
// content-addressed key (`{prefix}/{sha256}.{ext}`). That makes each object
// immutable, so it can be cached locally, and makes the integrity check free:
// the key is the expected digest.

/// Upload a public object under `{prefix}/{sha256}.{ext}`; returns the key for
/// the caller to persist.
pub async fn upload_public_file(
    prefix: String,
    data: Vec<u8>,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<UploadResult> {
    if data.len() as u64 > pollis_api::broker::R2_PUBLIC_IMAGE_MAX_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "image is {} bytes; the limit is {} bytes",
            data.len(),
            pollis_api::broker::R2_PUBLIC_IMAGE_MAX_BYTES
        )));
    }
    let hash = hex::encode(sha256_bytes(&data));
    let ext = ext_for_content_type(&content_type);
    let key = format!("{}/{hash}.{ext}", prefix.trim_matches('/'));

    let put_url =
        presign_r2_with_length(state, "put", &key, Some(data.len() as u64)).await?;
    let overlay = state.overlay_handle();
    r2_put_url(overlay.as_deref(), &put_url, data, &content_type).await?;
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);
    Ok(UploadResult { key, url })
}

/// The sha256 and extension a content-addressed public key commits to. `None`
/// for a legacy mutable key, which can't be cached safely.
fn public_key_digest(key: &str) -> Option<(String, String)> {
    let name = key.rsplit('/').next()?;
    let (hash, ext) = name.rsplit_once('.')?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return None;
    }
    Some((hash.to_string(), ext.to_string()))
}

/// Resolve a public object to a loopback URL for `<img src>`, caching it
/// encrypted at rest. `""` when it can't be served that way (legacy key, media
/// server not up); the frontend then uses the byte path.
pub async fn get_public_file_url(key: String, state: &Arc<AppState>) -> Result<String> {
    let Some((content_hash, ext)) = public_key_digest(&key) else {
        return Ok(String::new());
    };

    let port = *state.media_server_port.lock().await;
    let token = state.media_server_token.lock().await.clone();
    let (Some(port), Some(token)) = (port, token) else {
        return Ok(String::new());
    };
    let url = format!("http://127.0.0.1:{port}/{token}/{content_hash}");

    if find_cached_file(&content_hash).is_some() {
        return Ok(url);
    }

    let content_type = content_type_for_ext(&ext);
    let target = cache_file_path(&content_hash, content_type)?;

    let get_url = presign_r2(state, "get", &key).await?;
    let overlay = state.overlay_handle();
    // Over the per-file cap: the empty-string sentinel, with the transfer aborted
    // at the cap.
    let Some(bytes) = r2_get_url(overlay.as_deref(), &get_url, MEDIA_CACHE_MAX_FILE_BYTES).await?
    else {
        return Ok(String::new());
    };

    // Public objects are unencrypted, so the hash check is the only attestation.
    let actual = hex::encode(sha256_bytes(&bytes));
    if actual != content_hash {
        return Err(Error::Other(anyhow::anyhow!(
            "public object {key} failed its content-hash check (got {actual}); \
             refusing to render substituted bytes"
        )));
    }

    let db_key = {
        let guard = state.unlock.lock().await;
        match guard.as_ref() {
            Some(u) => u.db_key.to_vec(),
            None => {
                return Err(Error::Other(anyhow::anyhow!(
                    "cannot cache a public object without an active unlock"
                )))
            }
        }
    };
    // `Vec::from` moves a uniquely-owned `Bytes` allocation (#915).
    let encrypted = cache_encrypt(Vec::from(bytes), &db_key, content_hash.as_bytes())?;

    let dir = media_cache_dir()?;
    crate::private_fs::create_dir_all(&dir)
        .map_err(|e| Error::Other(anyhow::anyhow!("create cache dir: {e}")))?;

    // Atomic write: a half-written cache file would be served as a corrupt image.
    let mut tmp = target.clone();
    let final_ext = target.extension().and_then(|s| s.to_str()).unwrap_or("enc");
    tmp.set_extension(format!("{final_ext}.tmp"));
    crate::private_fs::write_async(&tmp, encrypted)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("write public cache: {e}")))?;
    if let Err(e) = tokio::fs::rename(&tmp, &target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(Error::Other(anyhow::anyhow!("rename public cache: {e}")));
    }

    enforce_cache_cap(&dir);
    Ok(url)
}

// Media upload (#1382): a file is sealed under
// `file_keys::derive_file_key(ring key, random file_id)` and stored at
// `media/{hex file_id}.enc`. Nothing the DS or R2 holds is key material; the
// plaintext SHA-256 travels only inside the MLS-encrypted reference and is
// checked after decrypt. There is no fallback to the retired convergent scheme.

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaUploadResult {
    pub key: String,
    pub url: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: usize,
    /// The object's random file id, hex. Named for the `_att` `hash` wire field; NOT
    /// derived from the content.
    pub content_hash: String,
    /// Which ring key sealed the file (`0` for a Vault file).
    pub key_version: i64,
    /// SHA-256 of the plaintext, hex; only ever inside the encrypted reference.
    pub sha256: String,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Which key an existing attachment is sealed under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentKeyScope {
    /// Ring key `key_version` of the conversation's MLS group.
    Conversation {
        conversation_id: String,
        key_version: i64,
    },
    /// Under the account's Vault file root.
    Vault,
}

/// Where a new upload is going, which decides its key.
pub(crate) enum UploadScope {
    /// The newest key of the conversation's ring.
    Conversation(String),
    Vault,
}

async fn upload_ring_key(
    state: &Arc<AppState>,
    scope: &UploadScope,
) -> Result<(i64, zeroize::Zeroizing<[u8; 32]>)> {
    let user_id = crate::commands::mls::current_user_id(state).await?;
    match scope {
        UploadScope::Conversation(conversation_id) => {
            let mls_id =
                crate::commands::mls::ds_reads::resolve_mls_group(state, conversation_id).await?;
            crate::commands::file_keys::upload_key(state, &mls_id, &user_id).await
        }
        UploadScope::Vault => Ok((
            0,
            crate::commands::vault::vault_file_root_key(state, &user_id).await?,
        )),
    }
}

async fn download_ring_key(
    state: &Arc<AppState>,
    scope: &AttachmentKeyScope,
) -> Result<zeroize::Zeroizing<[u8; 32]>> {
    let user_id = crate::commands::mls::current_user_id(state).await?;
    match scope {
        AttachmentKeyScope::Conversation {
            conversation_id,
            key_version,
        } => {
            let mls_id =
                crate::commands::mls::ds_reads::resolve_mls_group(state, conversation_id).await?;
            crate::commands::file_keys::key_at_version(state, &mls_id, &user_id, *key_version)
                .await
        }
        AttachmentKeyScope::Vault => {
            crate::commands::vault::vault_file_root_key(state, &user_id).await
        }
    }
}

fn parse_hex32(what: &str, value: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(value)
        .map_err(|e| Error::Other(anyhow::anyhow!("invalid {what}: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Other(anyhow::anyhow!("{what} must be 32 hex bytes")))
}

/// An upload's plaintext and who may release it. Both are let go before the PUT
/// (#915); `Staged` bytes belong to `commands::staging`, which keeps them until
/// the upload succeeds so a failed PUT can retry.
enum Plaintext {
    Owned(Vec<u8>),
    Staged(std::sync::Arc<zeroize::Zeroizing<Vec<u8>>>),
}

impl Plaintext {
    fn as_slice(&self) -> &[u8] {
        match self {
            Plaintext::Owned(v) => v,
            Plaintext::Staged(v) => v,
        }
    }

    /// Drop this call's hold so the PUT doesn't run with both buffers resident.
    fn release(self) {
        drop(self);
    }
}

/// Uploads that may hold their bytes at once (#1497): each keeps plaintext,
/// ciphertext and decoded pixels resident, and a 12-image Vault entry started
/// all at once exceeded what a phone allows.
const MAX_CONCURRENT_UPLOADS: usize = 3;

static UPLOAD_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_UPLOADS);

/// Wait for an upload slot, held for the upload's life. Taken only by the public
/// entry points, so never twice per upload.
async fn upload_slot() -> tokio::sync::SemaphorePermit<'static> {
    UPLOAD_SLOTS
        .acquire()
        .await
        .expect("the upload semaphore is never closed")
}

/// Upload a file from a path the user picked (picker, OS drop) to their Vault,
/// sealed under the Vault file key (#1382). Chat attachments go through the
/// outbox (`upload_outbox_bytes`); webview bytes with no path go through
/// [`upload_media_staged`].
pub async fn upload_media(
    path: String,
    filename: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    // Refuse an over-cap file from its metadata before reading it: buffering a 2 GB
    // pick just to say no is an OOM kill on a phone (#1491).
    let len = tokio::fs::metadata(&path)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("stat file {path}: {e}")))?
        .len();
    if len > pollis_api::broker::R2_MEDIA_MAX_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "attachment is {len} bytes; the limit is {} bytes",
            pollis_api::broker::R2_MEDIA_MAX_BYTES
        )));
    }

    let _slot = upload_slot().await;
    let data = tokio::fs::read(&path).await
        .map_err(|e| Error::Other(anyhow::anyhow!("read file {path}: {e}")))?;

    upload_plaintext(Plaintext::Owned(data), filename, content_type, UploadScope::Vault, state).await
}

/// Upload an attachment staged in memory (`commands::staging`). The staged bytes
/// are released only on success, so a failed upload can retry.
pub async fn upload_media_staged(
    staged_id: String,
    filename: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    let _slot = upload_slot().await;
    let bytes = crate::commands::staging::peek_staged(&staged_id).ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "no staged attachment {staged_id}: it was already uploaded, discarded, or the              session ended"
        ))
    })?;

    let result =
        upload_plaintext(Plaintext::Staged(bytes), filename, content_type, UploadScope::Vault, state).await;
    if result.is_ok() {
        crate::commands::staging::discard_staged(&staged_id);
    }
    result
}

/// Upload bytes the durable outbox kept for an unsent message (#1335).
pub(crate) async fn upload_outbox_bytes(
    data: Vec<u8>,
    filename: String,
    content_type: String,
    conversation_id: &str,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    // Test harness: no R2 in the flows world, so return what an upload would
    // without the PUT. The ring key is still resolved, so a missing file key fails
    // as in production.
    #[cfg(feature = "test-harness")]
    if crate::commands::messages::test_hooks::uploads_stubbed() {
        let (key_version, _key) =
            upload_ring_key(state, &UploadScope::Conversation(conversation_id.to_string())).await?;
        let mut file_id = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut file_id);
        let content_hash = hex::encode(file_id);
        let key = format!("media/{content_hash}.enc");
        return Ok(MediaUploadResult {
            url: format!("{}/{key}", state.config.r2_endpoint.trim_end_matches('/')),
            key,
            filename,
            content_type,
            size_bytes: data.len(),
            content_hash,
            key_version,
            sha256: hex::encode(sha256_bytes(&data)),
            blurhash: None,
            width: None,
            height: None,
        });
    }
    let _slot = upload_slot().await;
    upload_plaintext(
        Plaintext::Owned(data),
        filename,
        content_type,
        UploadScope::Conversation(conversation_id.to_string()),
        state,
    )
    .await
}

struct Sealed {
    /// Plaintext SHA-256, hex (#879).
    sha256: String,
    image_meta: Option<anyhow::Result<(String, u32, u32)>>,
    ciphertext: Vec<u8>,
}

/// Hash, measure and encrypt an upload's plaintext on the blocking pool (#1497).
/// Seconds of CPU for a phone photo; run inline across several uploads it parked
/// every runtime worker, starving the HTTP requests they drive.
async fn seal_off_runtime(
    data: Plaintext,
    is_image: bool,
    enc_key: zeroize::Zeroizing<[u8; 32]>,
    enc_nonce: [u8; 12],
) -> Result<Sealed> {
    tokio::task::spawn_blocking(move || {
        // The digest rides inside the encrypted reference so the recipient can confirm
        // the exact bytes (#879).
        let sha256 = hex::encode(sha256_bytes(data.as_slice()));
        let image_meta = if is_image {
            Some(compute_image_meta(data.as_slice()))
        } else {
            None
        };
        let ciphertext = encrypt_chunked(data.as_slice(), &enc_key, &enc_nonce);
        drop(enc_key);
        // Release the plaintext before the presign and PUT, the longest steps (#915).
        data.release();
        Sealed {
            sha256,
            image_meta,
            ciphertext,
        }
    })
    .await
    .map_err(|e| Error::Other(anyhow::anyhow!("upload preparation failed: {e}")))
}

/// The shared upload core: seal under a fresh per-file key from `scope`'s ring,
/// PUT to a random object name, register the object.
async fn upload_plaintext(
    data: Plaintext,
    filename: String,
    content_type: String,
    scope: UploadScope,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    let size_bytes = data.as_slice().len();

    // Refuse over-cap before encrypting; the DS refuses to sign a PUT above the
    // same shared bound (`R2_MEDIA_MAX_BYTES`), which leaves room for the per-chunk
    // tags.
    if size_bytes as u64 > pollis_api::broker::R2_MEDIA_MAX_BYTES {
        return Err(Error::Other(anyhow::anyhow!(
            "attachment is {size_bytes} bytes; the limit is {} bytes",
            pollis_api::broker::R2_MEDIA_MAX_BYTES
        )));
    }

    // Key first: a ring this device can't reach yet must fail before any work.
    let (key_version, ring_key) = upload_ring_key(state, &scope).await?;

    // A random file id names the object and salts the file key; not key material on
    // its own.
    let mut file_id = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut file_id);
    let content_hash = hex::encode(file_id);
    let r2_key = format!("media/{content_hash}.enc");
    let r2_url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), r2_key);
    let (enc_key, enc_nonce) = crate::commands::file_keys::derive_file_key(&ring_key, &file_id);
    drop(ring_key);

    let is_image = content_type.starts_with("image/");
    let Sealed { sha256, image_meta, ciphertext } =
        seal_off_runtime(data, is_image, enc_key, enc_nonce).await?;

    let (blurhash, width, height) = match image_meta {
        Some(Ok((bh, w, h))) => (Some(bh), Some(w), Some(h)),
        Some(Err(e)) => {
            crate::diag!("[upload_media] image meta failed ({content_type}): {e}");
            (None, None, None)
        }
        None => (None, None, None),
    };

    let put_url =
        presign_r2_with_length(state, "put", &r2_key, Some(ciphertext.len() as u64)).await?;
    let overlay = state.overlay_handle();
    r2_put_url(overlay.as_deref(), &put_url, ciphertext, "application/octet-stream").await?;

    // Register the object for DS reference counting; the send path registers the
    // `(file id, message_id)` reference.
    let body = pollis_api::messages::AttachmentRegisterBody::ObjectOnly {
        content_hash: content_hash.clone(),
        r2_key: r2_key.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(MediaUploadResult {
        key: r2_key,
        url: r2_url,
        filename,
        content_type,
        size_bytes,
        content_hash,
        key_version,
        sha256,
        blurhash,
        width,
        height,
    })
}

/// Download and decrypt a media attachment. `content_hash` (the file id),
/// `sha256` and `scope` all come from the MLS-encrypted reference.
pub async fn download_media(
    r2_key: String,
    content_hash: String,
    sha256: String,
    scope: AttachmentKeyScope,
    state: &Arc<AppState>,
) -> Result<Vec<u8>> {
    let file_id = parse_hex32("file id", &content_hash)?;
    let expected_sha = parse_hex32("attachment sha256", &sha256)?;

    let ring_key = download_ring_key(state, &scope).await?;
    let (enc_key, enc_nonce) = crate::commands::file_keys::derive_file_key(&ring_key, &file_id);
    drop(ring_key);

    let get_url = presign_r2(state, "get", &r2_key).await?;
    let overlay = state.overlay_handle();
    // Buffered whole to decrypt and re-hash, so the cap stops a DS-chosen URL from
    // streaming forever.
    let ciphertext = match r2_get_url(overlay.as_deref(), &get_url, R2_MAX_DOWNLOAD_BYTES).await? {
        Some(bytes) => bytes,
        None => {
            return Err(Error::Other(anyhow::anyhow!(
                "attachment {r2_key} is over the download cap; refusing to buffer it"
            )))
        }
    };
    let plaintext = decrypt_chunked(&ciphertext, &enc_key, &enc_nonce)?;
    // Drop the ciphertext now so the hash check doesn't run with two full copies
    // resident (#915).
    drop(ciphertext);

    // AEAD proves a ring-key holder (any member) sealed it; the sender's digest
    // proves it is the bytes they meant (#879).
    let actual = sha256_bytes(&plaintext);
    if actual != expected_sha {
        return Err(Error::Other(anyhow::anyhow!(
            "attachment {r2_key} does not match the digest in its reference; \
             refusing to return substituted or corrupted bytes"
        )));
    }
    Ok(plaintext)
}

/// Save an attachment to a user-picked path, marked as a download so Gatekeeper
/// and SmartScreen treat it as from the internet (`crate::downloads`).
///
/// Returns what happened to the marker (`marked`, `unsupported`, `failed: …`). A
/// failed marker doesn't fail the save; the renderer surfaces it.
pub async fn save_media_to_path(
    r2_key: String,
    content_hash: String,
    sha256: String,
    scope: AttachmentKeyScope,
    target_path: String,
    state: &Arc<AppState>,
) -> Result<String> {
    let bytes = download_media(r2_key, content_hash, sha256, scope, state).await?;
    let path = std::path::PathBuf::from(&target_path);
    let marking = tokio::task::spawn_blocking(move || {
        crate::downloads::write_downloaded_file(&path, &bytes)
    })
    .await
    .map_err(|e| Error::Other(anyhow::anyhow!("save attachment: {e}")))?
    .map_err(|e| Error::Other(anyhow::anyhow!("write {target_path}: {e}")))?;

    Ok(match marking {
        crate::downloads::Marking::Marked => "marked".to_string(),
        crate::downloads::Marking::Unsupported => "unsupported".to_string(),
        crate::downloads::Marking::Failed(reason) => format!("failed: {reason}"),
    })
}

/// Resolve a media attachment to a loopback URL for `<img>`/`<audio>`/`<video>`,
/// caching it encrypted at rest. `""` for files over
/// `MEDIA_CACHE_MAX_FILE_BYTES`; the frontend then uses the in-memory byte path.
pub async fn get_media_url(
    r2_key: String,
    content_hash: String,
    content_type: String,
    sha256: String,
    scope: AttachmentKeyScope,
    state: &Arc<AppState>,
) -> Result<String> {
    let url = crate::media_server::loopback_url(state, &content_hash).await?;

    let target = cache_file_path(&content_hash, &content_type)?;

    // Cached: touch mtime for the LRU.
    if target.exists() {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&target) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return Ok(url);
    }

    // Per-hash lock so a second waiter finds the file instead of re-downloading.
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

    let bytes = download_media(r2_key, content_hash.clone(), sha256, scope, state).await?;

    if bytes.len() as u64 > MEDIA_CACHE_MAX_FILE_BYTES {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Ok(String::new());
    }

    let dir = media_cache_dir()?;
    if let Err(e) = crate::private_fs::create_dir_all(&dir) {
        in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
        return Err(Error::Other(anyhow::anyhow!("create cache dir: {e}")));
    }

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
    // `bytes` moves into the encryption (#915) rather than coexisting with a fresh
    // ciphertext.
    let encrypted = match cache_encrypt(bytes, &db_key, content_hash.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            in_flight().lock().expect("in-flight map poisoned").remove(&content_hash);
            return Err(e);
        }
    };

    // Evict before writing so the cache never peaks above the cap.
    let new_size = encrypted.len() as u64;
    let total = cache_total_bytes();
    if total.saturating_add(new_size) > MEDIA_CACHE_MAX_BYTES {
        enforce_cache_cap_to(&dir, MEDIA_CACHE_MAX_BYTES.saturating_sub(new_size));
    }

    let mut tmp = target.clone();
    let final_ext = target
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("enc");
    tmp.set_extension(format!("{final_ext}.tmp"));
    if let Err(e) = crate::private_fs::write_async(&tmp, encrypted).await {
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

// Cache-at-rest layout: `[12-byte random nonce][AES-256-GCM(plaintext)][16-byte
// tag]`, under a key from `db_key` and the content hash. Decrypted whole before
// serving; files are bounded well under the AES-GCM per-key limit. The salt
// domain and `db_key` IKM keep it apart from the upload-side file key.

const CACHE_HKDF_SALT: &[u8] = b"pollis-media-cache-v1";
const CACHE_NONCE_LEN: usize = 12;

fn derive_cache_key(db_key: &[u8], info: &[u8]) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let hk = Hkdf::<Sha256>::new(Some(CACHE_HKDF_SALT), db_key);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key)
        .expect("HKDF expand for cache key should never fail");
    key
}

/// Encrypt cache bytes: `[12-byte nonce][ciphertext+tag]`.
///
/// Takes the plaintext by value and encrypts in place, splicing the nonce on
/// with a rotate, so a large attachment isn't held three times (#915; asserted
/// in `tests/attachment_memory.rs`). `decrypt_chunked`'s output already has the
/// spare capacity, so the real path allocates nothing. This is the client's
/// largest allocation, and mobile ships it too.
pub fn cache_encrypt(mut plaintext: Vec<u8>, db_key: &[u8], info: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::{aead::{AeadInPlace, KeyInit}, Aes256Gcm, Key, Nonce};
    use rand::RngCore;

    let key = derive_cache_key(db_key, info);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; CACHE_NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    // Reserve tag and nonce space so nothing below reallocates.
    plaintext.reserve_exact(CACHE_NONCE_LEN + 16);
    cipher
        .encrypt_in_place(Nonce::from_slice(&nonce_bytes), &[], &mut plaintext)
        .map_err(|_| Error::Other(anyhow::anyhow!("media cache encrypt failed")))?;

    // `rotate_right` is an in-place memmove.
    plaintext.extend_from_slice(&nonce_bytes);
    plaintext.rotate_right(CACHE_NONCE_LEN);
    Ok(plaintext)
}

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

/// Delete an R2 object via a DS-minted presigned DELETE. A 404 counts as success.
pub(crate) async fn delete_r2_object(
    state: &Arc<AppState>,
    r2_key: &str,
) -> Result<()> {
    let delete_url = presign_r2(state, "delete", r2_key).await?;
    let overlay = state.overlay_handle();
    r2_delete_url(overlay.as_deref(), &delete_url).await
}

/// Chunk size for chunked AES-256-GCM.
const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

const TAG_SIZE: usize = 16;

/// Chunked AES-256-GCM: per-chunk nonce = base nonce XOR the LE chunk index
/// (first 4 bytes). Output is each chunk's ciphertext + 16-byte tag, concatenated.
pub(crate) fn encrypt_chunked(data: &[u8], key: &[u8; 32], base_nonce: &[u8; 12]) -> Vec<u8> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
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

pub(crate) fn decrypt_chunked(ciphertext: &[u8], key: &[u8; 32], base_nonce: &[u8; 12]) -> Result<Vec<u8>> {
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

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    use sha2::{Sha256, Digest};
    Sha256::digest(data).into()
}

/// Longest edge of the image a blurhash is computed from.
const BLURHASH_SOURCE_MAX: u32 = 64;

fn compute_image_meta(data: &[u8]) -> anyhow::Result<(String, u32, u32)> {
    use image::GenericImageView;
    let img = image::load_from_memory(data)
        .map_err(|e| anyhow::anyhow!("image decode: {e}"))?;
    let (width, height) = img.dimensions();
    // A 4x3 blurhash from a thumbnail matches the full image's, and encoding the
    // full frame cost seconds per phone photo (#1497).
    let small = img.thumbnail(BLURHASH_SOURCE_MAX, BLURHASH_SOURCE_MAX).to_rgba8();
    let hash = blurhash::encode(4, 3, small.width(), small.height(), small.as_raw())
        .map_err(|e| anyhow::anyhow!("blurhash: {e:?}"))?;
    Ok((hash, width, height))
}

// The client holds no R2 credentials (`docs/secrets-broker.md`): every access
// uses a short-lived presigned URL from the DS's device-signed
// `/v1/r2/presign`, fetched with no auth headers.

/// A presigned URL checked to point at first-party R2.
///
/// The DS chooses the string and the client fetches it unauthenticated, so an
/// unchecked URL would let a hostile DS aim uploads and downloads at any host.
/// The only constructor is [`PresignedUrl::parse`] and the request helpers take
/// only this type, so a raw string can't reach a request builder.
pub(crate) struct PresignedUrl(String);

impl PresignedUrl {
    /// Accept `url` only if its whole origin (scheme included, so `http://` can't
    /// pass for `https://`) is `r2_endpoint` or `r2_public_url`. An empty configured
    /// value allows nothing.
    fn parse(url: String, config: &crate::config::Config) -> Result<Self> {
        let origin = origin_of(&url);
        let allowed = [
            origin_of(&config.r2_endpoint),
            origin_of(&config.r2_public_url),
        ];
        match origin {
            Some(o) if allowed.iter().flatten().any(|a| *a == o) => Ok(Self(url)),
            _ => Err(Error::Other(anyhow::anyhow!(
                "refusing a presigned URL that is not on the configured R2 origin"
            ))),
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// `scheme://host[:port]`, lowercased; `None` if either half is missing, so a
/// relative URL never matches.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // The host follows the last `@`, as a URL parser resolves it, so
    // `https://r2.example.com@evil.test/` doesn't read as the R2 host.
    let host = authority.rsplit('@').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

/// Why a host's R2 config can never move media, or `None` if both values name an
/// origin. The mobile bridge calls this at `init_pollis` so a build missing its
/// R2 values fails at launch naming the variable, rather than showing every
/// attachment as "Image unavailable".
pub(crate) fn r2_config_problem(r2_endpoint: &str, r2_public_url: &str) -> Option<String> {
    let missing: Vec<&str> = [
        ("r2_endpoint (EXPO_PUBLIC_R2_ENDPOINT)", r2_endpoint),
        ("r2_public_url (EXPO_PUBLIC_R2_PUBLIC_URL)", r2_public_url),
    ]
    .into_iter()
    .filter(|(_, value)| origin_of(value.trim()).is_none())
    .map(|(name, _)| name)
    .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "{} not set to an R2 URL; every attachment upload and download would be refused. \
         Build with the R2 values for the Delivery Service this build talks to.",
        missing.join(" and ")
    ))
}

/// Test harness only: a presigned GET minted by this client's DS session, for the
/// vault soak suite (#1491).
#[cfg(feature = "test-harness")]
pub async fn test_presign_get(state: &Arc<AppState>, key: &str) -> Result<String> {
    Ok(presign_r2(state, "get", key).await?.as_str().to_string())
}

/// Ask the DS to presign an R2 `operation` (`"get"`/`"put"`/`"delete"`) on `key`.
async fn presign_r2(
    state: &Arc<AppState>,
    operation: &str,
    key: &str,
) -> Result<PresignedUrl> {
    presign_r2_with_length(state, operation, key, None).await
}

/// [`presign_r2`] declaring the exact byte count a `put` carries; the DS signs
/// `content-length` into the URL so R2 refuses any other size. Required on every
/// `put`: only R2 counts the bytes, so the cap must be in the signature.
pub(crate) async fn presign_r2_with_length(
    state: &Arc<AppState>,
    operation: &str,
    key: &str,
    content_length: Option<u64>,
) -> Result<PresignedUrl> {
    let body = pollis_api::broker::R2PresignBody {
        operation: operation.to_string(),
        key: key.to_string(),
        // The DS signs only `host`; Content-Type is set at upload.
        content_type: None,
        content_length,
        // No-auth fallback identity (`broker::resolve_user`); ignored when signed.
        user_id: Some(crate::commands::mls::current_user_id(state).await?),
    };
    let parsed = crate::commands::mls::ds_post_json(state, &body).await?;
    PresignedUrl::parse(parsed.url, &state.config)
}

/// At most a kilobyte of a failed response's body: an error path can stream
/// forever just like a 200.
async fn error_snippet(mut resp: reqwest::Response) -> String {
    const SNIPPET_MAX: usize = 1024;
    let mut buf = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let room = SNIPPET_MAX - buf.len();
        buf.extend_from_slice(&chunk[..room.min(chunk.len())]);
        if buf.len() >= SNIPPET_MAX {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// PUT `data` to a presigned URL, through the overlay when on (§14.2).
/// Content-Type is unsigned by design.
pub(crate) async fn r2_put_url(
    overlay: Option<&pollis_relay::OverlayHandle>,
    url: &PresignedUrl,
    data: Vec<u8>,
    content_type: &str,
) -> Result<()> {
    let resp = crate::net::overlay::http_client(overlay)
        .put(url.as_str())
        .header("Content-Type", content_type)
        .body(data)
        .send()
        .await
        // `without_url`: a presigned URL is a write credential and must not reach logs.
        .map_err(|e| e.without_url())
        .inspect_err(|e| {
            crate::diag!(
                "[r2] PUT transport error: {}",
                crate::commands::mls::ds_client::error_chain(e)
            );
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        crate::diag!("[r2] PUT → {status}");
        let body = error_snippet(resp).await;
        return Err(Error::Other(anyhow::anyhow!("R2 upload failed: {} — {}", status, body)));
    }
    Ok(())
}

/// GET a presigned URL, refusing to buffer more than `max_bytes`. Through the
/// overlay when on.
///
/// `Ok(None)` means the transfer was aborted at the cap, not read and then
/// measured: a check after `resp.bytes()` comes too late to stop a URL that
/// streams forever. Returns `Bytes` so callers don't each pay a copy (#915).
pub(crate) async fn r2_get_url(
    overlay: Option<&pollis_relay::OverlayHandle>,
    url: &PresignedUrl,
    max_bytes: u64,
) -> Result<Option<bytes::Bytes>> {
    let mut resp = crate::net::overlay::http_client(overlay)
        .get(url.as_str())
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = error_snippet(resp).await;
        return Err(Error::Other(anyhow::anyhow!("R2 download failed: {} — {}", status, body)));
    }
    // A declared length over the cap is refused up front; advisory, the loop below
    // is what holds.
    if resp.content_length().is_some_and(|n| n > max_bytes) {
        return Ok(None);
    }
    let mut buf = bytes::BytesMut::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(Some(buf.freeze()))
}

/// DELETE a presigned URL; a 404 counts as success. Through the overlay when on.
async fn r2_delete_url(
    overlay: Option<&pollis_relay::OverlayHandle>,
    url: &PresignedUrl,
) -> Result<()> {
    let resp = crate::net::overlay::http_client(overlay)
        .delete(url.as_str())
        .send()
        .await?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 404 {
        return Ok(());
    }
    let body = error_snippet(resp).await;
    Err(Error::Other(anyhow::anyhow!("R2 delete failed: {} — {}", status, body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// `CACHE_DIR_WALKS` is process-global, so every sweeping test holds this.
    static WALK_COUNTER: Mutex<()> = Mutex::new(());

    fn serialise_sweeps() -> std::sync::MutexGuard<'static, ()> {
        WALK_COUNTER.lock().unwrap_or_else(|e| e.into_inner())
    }

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    /// A private per-user directory under the shared root; the cap sweep refuses to
    /// evict anywhere else.
    fn scratch_dir(tag: &str) -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = test_cache_root().join(format!("r2-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// A directory outside the cache root with a file and a populated subdirectory,
    /// returning the paths that must survive. Inside the private temp dir, so a
    /// broken guard can do no real damage.
    fn outside_dir(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = test_cache_temp().join(format!("outside-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Documents")).expect("create outside dir");
        let file = dir.join("notes.txt");
        let nested = dir.join("Documents").join("thesis.txt");
        std::fs::write(&file, b"not cache").expect("seed outside file");
        std::fs::write(&nested, b"not cache either").expect("seed outside nested file");
        (dir, file, nested)
    }

    /// Write a file with a fixed mtime so "oldest first" is deterministic.
    fn write_aged(dir: &Path, name: &str, bytes: usize, age_secs: u64) {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; bytes]).expect("write cache file");
        let when =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 - age_secs);
        let f = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("reopen for mtime");
        f.set_modified(when).expect("set mtime");
    }

    /// The cap evicts oldest first and stops at the target (#930).
    #[test]
    fn the_cap_evicts_oldest_first_and_stops_at_the_target() {
        let _serial = serialise_sweeps();
        let dir = scratch_dir("evict");
        // 400 bytes: a=100 (oldest), b=100, c=200 (newest).
        write_aged(&dir, "a.png.enc", 100, 300);
        write_aged(&dir, "b.png.enc", 100, 200);
        write_aged(&dir, "c.png.enc", 200, 100);

        enforce_cache_cap_to(&dir, 250);

        assert!(!dir.join("a.png.enc").exists(), "the oldest entry must go first");
        assert!(!dir.join("b.png.enc").exists(), "eviction must continue until under target");
        assert!(
            dir.join("c.png.enc").exists(),
            "eviction must stop as soon as it is under the target, not empty the cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cap_ignores_in_progress_writes() {
        let _serial = serialise_sweeps();
        let dir = scratch_dir("tmp");
        write_aged(&dir, "a.png.enc", 400, 300);
        write_aged(&dir, "b.png.enc.tmp", 400, 400);

        enforce_cache_cap_to(&dir, 100);

        assert!(!dir.join("a.png.enc").exists());
        assert!(
            dir.join("b.png.enc.tmp").exists(),
            "a .tmp is a half-written file someone is still writing, not an eviction candidate"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Proves the counter counts, so the "0 walks" guard can't pass on a broken
    /// counter.
    #[test]
    fn the_walk_counter_counts_one_walk_per_sweep() {
        let _serial = serialise_sweeps();
        let dir = scratch_dir("counter");
        write_aged(&dir, "a.png.enc", 10, 10);

        let before = cache_dir_walks();
        enforce_cache_cap_to(&dir, u64::MAX);
        enforce_cache_cap_to(&dir, u64::MAX);
        assert_eq!(
            cache_dir_walks() - before,
            2,
            "each sweep must register exactly one directory walk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GUARD (#930): window focus must not walk the media cache. A source scan,
    /// because the call lived in a Tauri event closure no test can dispatch.
    #[test]
    fn guard_window_focus_does_not_sweep_the_media_cache() {
        let shell = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../src-tauri/src/lib.rs"));

        let focus_arm_start = shell
            .find("tauri::WindowEvent::Focused(true)")
            .expect("the shell must still have a window-focus arm for this guard to mean anything");
        let rest = &shell[focus_arm_start..];
        let focus_arm_end = rest
            .find("tauri::WindowEvent::CloseRequested")
            .unwrap_or(rest.len());
        let focus_arm = &rest[..focus_arm_end];

        assert!(
            !focus_arm.contains("enforce_cache_cap"),
            "the media-cache sweep is back on window focus (#930): it walks the whole \
             cache directory, so it costs more the longer the app has been used and \
             nothing the user did on that alt-tab made the cache bigger. The cap is \
             enforced on cache WRITES in this module instead."
        );
        assert!(
            !focus_arm.contains("cache_total_bytes"),
            "totalling the cache on focus is the same directory walk under another name (#930)"
        );
    }

    /// GUARD (#930): the public focus-time entry point stays deleted.
    #[test]
    fn guard_no_public_entry_point_for_a_focus_time_sweep() {
        // Scan only the module: the assertion message names the symbol.
        let source = include_str!("r2.rs");
        let module = &source[..source.find("mod tests {").unwrap_or(source.len())];
        let banned = format!("pub fn {}", "enforce_cache_cap_now");
        assert!(
            !module.contains(&banned),
            "`enforce_cache_cap_now` existed only to be called from window focus (#930); \
             re-exporting it re-opens that door — the cap belongs to the write path"
        );
    }

    fn seed_cache_entry(user: &str, name: &str) -> PathBuf {
        let dir = test_cache_root().join(user);
        crate::private_fs::create_dir_all(&dir).expect("create user cache dir");
        let path = dir.join(name);
        std::fs::write(&path, b"decrypted-media-bytes").expect("seed cache entry");
        path
    }

    /// The wipe used to resolve the ambient user, which every caller had wrong, so
    /// it removed nothing (#1000). This names the user and points the ambient one
    /// elsewhere first.
    #[test]
    fn wiping_one_user_removes_that_user_s_files() {
        let _serial = serialise_sweeps();
        let entry = seed_cache_entry("wipe-user-a", "aaaa.png.enc");
        assert!(entry.exists());

        set_cache_user(None);

        clear_media_cache(CacheScope::User("wipe-user-a"));

        assert!(
            !entry.exists(),
            "the wipe left the user's cached media on disk"
        );
    }

    #[test]
    fn wiping_one_user_leaves_another_user_s_files() {
        let _serial = serialise_sweeps();
        let mine = seed_cache_entry("wipe-user-b", "bbbb.png.enc");
        let theirs = seed_cache_entry("wipe-user-c", "cccc.png.enc");

        clear_media_cache(CacheScope::User("wipe-user-b"));

        assert!(!mine.exists());
        assert!(
            theirs.exists(),
            "wiping one user's cache emptied another user's"
        );
    }

    /// `Everything` reaches into the per-user directories, which are the root's
    /// entries.
    #[test]
    fn wiping_everything_reaches_inside_the_per_user_directories() {
        let _serial = serialise_sweeps();
        let one = seed_cache_entry("wipe-all-d", "dddd.png.enc");
        let two = seed_cache_entry("wipe-all-e", "eeee.png.enc");

        clear_media_cache(CacheScope::Everything);

        assert!(!one.exists(), "a per-user cache file survived a full wipe");
        assert!(!two.exists(), "a per-user cache file survived a full wipe");
        assert!(
            test_cache_root().exists(),
            "the cache root itself must stay so the next sign-in need not recreate it"
        );
    }

    // `user_id` comes from the DS and reaches `clear_media_cache`; with
    // `root.join(user_id)`, an absolute id replaced the root, `..` escaped it and
    // `""` named the root itself.

    /// A wipe scoped to a non-plain id deletes nothing anywhere.
    #[test]
    fn a_user_id_that_is_not_a_plain_component_wipes_nothing() {
        let _serial = serialise_sweeps();
        let (outside, file, nested) = outside_dir("wipe");
        let other = seed_cache_entry("wipe-user-f", "ffff.png.enc");
        let outside_name = outside.file_name().unwrap().to_str().unwrap().to_string();

        clear_media_cache(CacheScope::User(outside.to_str().unwrap()));
        clear_media_cache(CacheScope::User(&format!("../{outside_name}")));
        clear_media_cache(CacheScope::User(&format!("wipe-user-f/../../{outside_name}")));
        clear_media_cache(CacheScope::User(""));
        clear_media_cache(CacheScope::User(".."));
        clear_media_cache(CacheScope::User("."));

        assert!(file.exists(), "the wipe deleted a file outside the cache root");
        assert!(nested.exists(), "the wipe recursed into a directory outside the cache root");
        assert!(
            other.exists(),
            "a malformed user id must not fall back to wiping every user's cache"
        );
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The deletion primitive refuses anything outside the root, for future callers.
    #[test]
    fn remove_dir_contents_refuses_anything_outside_the_root() {
        let _serial = serialise_sweeps();
        let (outside, file, nested) = outside_dir("primitive");

        remove_dir_contents(test_cache_root(), &outside);
        remove_dir_contents(test_cache_root(), test_cache_temp());

        assert!(file.exists());
        assert!(nested.exists());
        assert!(test_cache_root().exists(), "the parent sweep removed the root itself");
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// Eviction outside the root, or at the root itself, evicts nothing.
    #[test]
    fn the_cap_refuses_to_evict_outside_a_per_user_directory() {
        let _serial = serialise_sweeps();
        let (outside, file, nested) = outside_dir("cap");
        write_aged(&outside, "old.png.enc", 400, 300);
        let in_root = test_cache_root().join("loose.png.enc");
        std::fs::write(&in_root, vec![0u8; 400]).expect("seed a file at the root");

        enforce_cache_cap_to(&outside, 0);
        enforce_cache_cap_to(test_cache_root(), 0);

        assert!(file.exists());
        assert!(nested.exists());
        assert!(outside.join("old.png.enc").exists(), "the cap evicted outside the cache root");
        assert!(in_root.exists(), "the cap swept the root itself");
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_file(&in_root);
    }

    /// Only a plain id produces a directory, a direct child of the root.
    #[test]
    fn only_a_plain_id_resolves_to_a_cache_directory() {
        let root = Path::new("/srv/pollis/media-cache");
        assert_eq!(
            user_cache_dir(root, "01JCACHEUSERLIFECYCLE0000"),
            Some(root.join("01JCACHEUSERLIFECYCLE0000"))
        );
        assert_eq!(user_cache_dir(root, "_anon"), Some(root.join("_anon")));
        for bad in ["", "/home/alice", "..", ".", "../x", "a/b", "a\\b", "a.b", "a b"] {
            assert_eq!(user_cache_dir(root, bad), None, "{bad:?} must not resolve");
        }
    }

    // The DS picks the presigned URL, so origin and size are enforced here.

    fn test_config(r2_endpoint: &str) -> crate::config::Config {
        let mut config = crate::config::Config::for_test().expect("test config");
        config.r2_endpoint = r2_endpoint.to_string();
        config.r2_public_url = "https://cdn.pollis.test".to_string();
        config
    }

    /// A URL off the configured R2 origin is refused, including lookalikes:
    /// userinfo, a suffix host, an `http://` downgrade, a relative URL.
    #[test]
    fn a_presigned_url_off_the_configured_origin_is_refused() {
        let config = test_config("https://acct.r2.cloudflarestorage.com");
        for bad in [
            "https://evil.test/media/x.enc?sig=1",
            "https://acct.r2.cloudflarestorage.com.evil.test/x",
            "https://evil.test@acct.r2.cloudflarestorage.com.evil.test/x",
            "http://acct.r2.cloudflarestorage.com/x",
            "https://acct.r2.cloudflarestorage.com:8443/x",
            "file:///etc/passwd",
            "/media/x.enc",
            "",
        ] {
            assert!(
                PresignedUrl::parse(bad.to_string(), &config).is_err(),
                "{bad:?} must not be fetchable"
            );
        }
    }

    /// Both configured origins are accepted, and the URL passes through verbatim.
    #[test]
    fn a_presigned_url_on_a_configured_origin_is_accepted_verbatim() {
        let config = test_config("https://acct.r2.cloudflarestorage.com");
        for good in [
            "https://acct.r2.cloudflarestorage.com/bucket/media/x.enc?X-Amz-Signature=abc",
            "https://ACCT.r2.CloudflareStorage.com/bucket/x",
            "https://cdn.pollis.test/emoji/x.webp",
        ] {
            let parsed = PresignedUrl::parse(good.to_string(), &config)
                .unwrap_or_else(|_| panic!("{good:?} must be fetchable"));
            assert_eq!(parsed.as_str(), good, "the URL must be passed through byte for byte");
        }
    }

    /// An unconfigured build allows nothing.
    #[test]
    fn an_unconfigured_r2_allows_no_url_at_all() {
        let config = crate::config::Config::for_test().expect("test config");
        assert!(PresignedUrl::parse("https://evil.test/x".into(), &config).is_err());
        assert!(PresignedUrl::parse("".into(), &config).is_err());
    }

    /// Missing R2 origins are named at init.
    #[test]
    fn an_r2_config_without_origins_is_named_at_init() {
        let both = r2_config_problem("", "").expect("empty config must be refused");
        assert!(both.contains("EXPO_PUBLIC_R2_ENDPOINT"), "{both}");
        assert!(both.contains("EXPO_PUBLIC_R2_PUBLIC_URL"), "{both}");

        let endpoint_only = r2_config_problem("https://acct.r2.cloudflarestorage.com/bucket", " ")
            .expect("a missing public URL must be refused");
        assert!(!endpoint_only.contains("EXPO_PUBLIC_R2_ENDPOINT"), "{endpoint_only}");
        assert!(endpoint_only.contains("EXPO_PUBLIC_R2_PUBLIC_URL"), "{endpoint_only}");

        assert!(r2_config_problem("acct.r2.cloudflarestorage.com", "https://cdn.pollis.test").is_some());
        assert_eq!(
            r2_config_problem("https://acct.r2.cloudflarestorage.com/bucket", "https://cdn.pollis.test"),
            None
        );
    }

    /// A minimal HTTP/1.1 server answering every request with `total` bytes and no
    /// `Content-Length`, so a length pre-check can't help. Returns its origin and a
    /// counter of bytes written.
    async fn streaming_stub(total: usize) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&written);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let mut scratch = [0u8; 1024];
                    let _ = sock.read(&mut scratch).await;
                    if sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let block = vec![b'x'; 64 * 1024];
                    let mut sent = 0usize;
                    while sent < total {
                        let n = block.len().min(total - sent);
                        if sock.write_all(&block[..n]).await.is_err() {
                            break;
                        }
                        sent += n;
                        counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://{addr}"), written)
    }

    /// The transfer must be abandoned at the cap, not read whole and then measured.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_past_the_cap_is_abandoned_mid_transfer() {
        const TOTAL: usize = 64 * 1024 * 1024;
        const CAP: u64 = 32 * 1024;

        let (origin, written) = streaming_stub(TOTAL).await;
        let config = test_config(&origin);
        let url = PresignedUrl::parse(format!("{origin}/media/huge.enc"), &config)
            .expect("the stub is the configured origin");

        let got = r2_get_url(None, &url, CAP).await.expect("no transport error");
        assert!(got.is_none(), "a body over the cap must not be returned");

        // Socket buffers make an exact count meaningless: assert it stayed bounded.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let sent = written.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            sent < TOTAL / 4,
            "the download must stop near the cap; the server got to send {sent} of {TOTAL} bytes"
        );
    }

    /// A body under the cap comes back whole and unmodified.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_within_the_cap_is_returned_whole() {
        const TOTAL: usize = 200 * 1024;

        let (origin, _) = streaming_stub(TOTAL).await;
        let config = test_config(&origin);
        let url = PresignedUrl::parse(format!("{origin}/media/ok.enc"), &config).expect("origin");

        let got = r2_get_url(None, &url, 1024 * 1024)
            .await
            .expect("no transport error")
            .expect("under the cap");
        assert_eq!(got.len(), TOTAL);
        assert!(got.iter().all(|b| *b == b'x'), "the bytes must be the ones sent");
    }

    /// #1497: sealing must not occupy the async runtime; on a single-threaded runtime
    /// the ticker advances only if the seal runs elsewhere.
    #[tokio::test]
    async fn sealing_an_upload_leaves_the_runtime_free() {
        let ticks = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticker = {
            let (ticks, stop) = (Arc::clone(&ticks), Arc::clone(&stop));
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    ticks.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        tokio::task::yield_now().await;

        let data = Plaintext::Owned(vec![7u8; 48 * 1024 * 1024]);
        let started = std::time::Instant::now();
        let sealed = seal_off_runtime(data, false, zeroize::Zeroizing::new([1u8; 32]), [2u8; 12])
            .await
            .expect("seal");
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        ticker.await.unwrap();

        assert!(sealed.image_meta.is_none());
        assert!(sealed.ciphertext.len() > 48 * 1024 * 1024);
        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "the seal finished in {elapsed:?}: too fast to tell whether it blocked"
        );
        assert!(
            ticks.load(Ordering::Relaxed) >= 3,
            "the runtime was blocked for the whole {elapsed:?} seal"
        );
    }

    /// The blurhash comes from a thumbnail (#1497), but reported dimensions are the
    /// original's.
    #[test]
    fn image_meta_reports_the_original_dimensions() {
        let img = image::RgbImage::from_fn(1200, 900, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encode");
        let (hash, w, h) = compute_image_meta(&bytes).expect("meta");
        assert_eq!((w, h), (1200, 900));
        // A 4x3 blurhash: 1 size + 1 max-AC + 4 DC + 2 per AC (11).
        assert_eq!(hash.len(), 1 + 1 + 4 + 2 * 11);
    }
}
