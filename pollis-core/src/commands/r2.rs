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

/// The one cache root every test in this binary shares.
///
/// [`MEDIA_CACHE_DIR`] is a `OnceLock` — production installs it once at
/// startup — so a test cannot have a root of its own. Tests scope themselves by
/// using their own user directory underneath this one instead.
#[cfg(test)]
pub(crate) fn test_cache_root() -> &'static Path {
    let root = MEDIA_CACHE_DIR.get_or_init(|| {
        std::env::temp_dir().join(format!("pollis-cache-root-{}", std::process::id()))
    });
    let _ = crate::private_fs::create_dir_all(root);
    root
}

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
    let _ = crate::private_fs::create_dir_all(&path);
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

/// `pub(crate)` so `commands::emoji` writes custom-emoji bytes into the SAME
/// on-disk cache under the same naming scheme. That is what lets the loopback
/// media server serve an emoji with no changes at all — it resolves
/// `/{token}/{hash}` by scanning for `<hash>.<ext>.enc`, and does not care which
/// subsystem put the file there.
pub(crate) fn cache_file_path(content_hash: &str, content_type: &str) -> Result<PathBuf> {
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

/// Counts every full walk of the media-cache directory.
///
/// Exists so "this code path does not stat the whole cache" can be asserted as
/// a number rather than timed (#930). Test-only; there is no counter in a
/// release build. Bumped by the three functions that enumerate the whole
/// directory — `enforce_cache_cap_to`, `cache_total_bytes`, `clear_media_cache`
/// — and not by `find_cached_file`, which short-circuits on its first match.
#[cfg(any(test, feature = "test-harness"))]
static CACHE_DIR_WALKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times the media-cache directory has been walked in this process.
#[cfg(any(test, feature = "test-harness"))]
pub fn cache_dir_walks() -> u64 {
    CACHE_DIR_WALKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Called at the top of every function that enumerates the cache directory.
#[inline]
fn note_cache_dir_walk() {
    #[cfg(any(test, feature = "test-harness"))]
    CACHE_DIR_WALKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Stat every file in the cache; if total size exceeds the cap, delete by
/// oldest mtime first until we're under.
///
/// **Driven by cache mutation, never by the UI (#930).** Every path that adds
/// bytes to the cache calls this immediately after its write, which is what
/// makes the cap hold and is also what catches files copied in from outside or
/// mtime-tampered. It used to be called on window focus as well: that walked
/// the entire directory on every alt-tab, scaling with the size of the cache
/// rather than with anything the user had done — and after #874 removed the
/// other focus-time costs it was the only work left there. A cache nobody is
/// writing to cannot grow, so there is nothing for a focus event to find.
fn enforce_cache_cap(dir: &Path) {
    enforce_cache_cap_to(dir, MEDIA_CACHE_MAX_BYTES);
}

/// Lower-bound variant: shrink the cache to at most `target_bytes` by
/// evicting oldest entries first. Used both by the regular cap-enforcer
/// and by the pre-write headroom check in `get_media_url`.
fn enforce_cache_cap_to(dir: &Path, target_bytes: u64) {
    note_cache_dir_walk();
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

// There is deliberately no `enforce_cache_cap_now()` (#930). It existed only
// so `src-tauri`'s `WindowEvent::Focused(true)` arm could re-run the sweep;
// see `enforce_cache_cap` above for why focus is the wrong trigger. Adding a
// public entry point back would re-open that door — the cap belongs to the
// write path.

/// Sum of all cached file sizes. Used to gate downloads against the cap
/// *before* writing new bytes, so the cache never peaks above the cap.
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
/// The scope is a PARAMETER, and that is the whole point. The wipe used to read
/// [`CURRENT_CACHE_USER`] itself, so which directory it hit depended on where in
/// a lifecycle sequence the caller happened to sit — and every caller sat on the
/// wrong side of it. `logout` called it after `unload_user_db` (which clears the
/// ambient user), and both PIN paths called it before the load that sets one, so
/// all three resolved `media-cache/_anon/`, an empty directory, and left the real
/// `media-cache/<user_id>/` untouched. Naming the user at the call site makes the
/// ordering unable to matter.
#[derive(Clone, Copy, Debug)]
pub enum CacheScope<'a> {
    /// One user's cache directory. What logout and both unlock paths want: this
    /// person's decrypted media stops being on disk, and a second client signed
    /// in as somebody else on the same machine keeps its own.
    User(&'a str),
    /// The whole cache root — every user's directory and the pre-sign-in `_anon`
    /// bucket. "Wipe this computer", and logout when the accounts index is too
    /// corrupt to say who was signed in.
    Everything,
}

/// Wipe the media cache, so decrypted media doesn't sit on disk past the session
/// that fetched it. The cache root itself stays, so a subsequent sign-in doesn't
/// have to re-create it.
///
/// Resolves the root directly rather than through `media_cache_dir()`: that
/// helper appends the ambient user, which is the coupling [`CacheScope`] exists
/// to remove.
pub fn clear_media_cache(scope: CacheScope<'_>) {
    note_cache_dir_walk();
    let root = match MEDIA_CACHE_DIR.get() {
        Some(r) => r,
        // No cache root was ever installed (pollis-tui, headless, tests), so
        // there is nothing on disk to wipe.
        None => return,
    };
    match scope {
        CacheScope::User(user_id) => remove_dir_contents(&root.join(user_id)),
        CacheScope::Everything => remove_dir_contents(root),
    }
}

/// Empty a directory of both files and subdirectories, keeping the directory.
fn remove_dir_contents(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Under `CacheScope::Everything` the entries are the per-user
        // directories, so a plain `remove_file` there would delete nothing.
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
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
    // The public byte-returning command: this is the boundary that owes an owned
    // `Vec`, so the conversion happens HERE rather than inside `r2_get_url`
    // where it used to cost every caller a copy (#915).
    Ok(r2_get_url(overlay.as_deref(), &get_url).await?.to_vec())
}

// ── Public objects (avatars, group icons) ─────────────────────────────────
//
// Avatars and group icons are stored in the CLEAR — they are public profile
// decoration, not message content. What they are not is stable: an avatar used
// to live at `avatars/{user_id}` and be overwritten in place, and a mutable key
// is precisely what makes a local cache unrepresentable-correctly. There is no
// way to know a cached copy is current without fetching it, so the client
// fetched every avatar again on every launch — as a JSON array of integers
// through the IPC, roughly three bytes on the wire per byte of image.
//
// So the key carries the hash of its own bytes. That makes the object
// immutable: a new avatar is a new key, which the profile row already
// publishes, so every viewer picks it up by normal cache invalidation and the
// on-disk copy of the old one is simply never asked for again. It also makes
// the integrity check free — the key IS the expected digest, exactly as
// custom emoji work (`commands::emoji::get_emoji_url`).

/// Upload a public object under a content-addressed key `{prefix}/{sha256}.{ext}`.
///
/// Returns the key so the caller can persist it (profile row, group row); the
/// bytes are reachable afterwards through [`get_public_file_url`].
pub async fn upload_public_file(
    prefix: String,
    data: Vec<u8>,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<UploadResult> {
    let hash = hex::encode(sha256_bytes(&data));
    let ext = ext_for_content_type(&content_type);
    let key = format!("{}/{hash}.{ext}", prefix.trim_matches('/'));

    let put_url = presign_r2(state, "put", &key).await?;
    let overlay = state.overlay_handle();
    r2_put_url(overlay.as_deref(), &put_url, data, &content_type).await?;
    let url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), key);
    Ok(UploadResult { key, url })
}

/// The sha256 a content-addressed public key commits to, plus its extension.
///
/// `None` for a LEGACY key (`avatars/{user_id}`, `group-icons/{id}/{ts}-{name}`)
/// written before objects were content-addressed. Those cannot be cached — the
/// bytes behind them can change without the key changing — so the caller falls
/// back to the byte path for them instead of serving a copy it cannot verify.
fn public_key_digest(key: &str) -> Option<(String, String)> {
    let name = key.rsplit('/').next()?;
    let (hash, ext) = name.rsplit_once('.')?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return None;
    }
    Some((hash.to_string(), ext.to_string()))
}

/// Resolve a public object to a loopback HTTP URL the webview can use directly
/// as `<img src>`, caching the bytes on disk encrypted at rest.
///
/// Returns `""` (the same empty-string sentinel [`get_media_url`] uses) when
/// this object cannot be served that way — a legacy non-content-addressed key,
/// or a media server that isn't up yet. The frontend falls back to the byte
/// path for those, which is what every caller did before #874.
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

    // Cache hit needs nothing else — the cached file's own extension is what
    // the media server serves it with.
    if find_cached_file(&content_hash).is_some() {
        return Ok(url);
    }

    let content_type = content_type_for_ext(&ext);
    let target = cache_file_path(&content_hash, content_type)?;

    let get_url = presign_r2(state, "get", &key).await?;
    let overlay = state.overlay_handle();
    let bytes = r2_get_url(overlay.as_deref(), &get_url).await?;

    if bytes.len() as u64 > MEDIA_CACHE_MAX_FILE_BYTES {
        return Ok(String::new());
    }

    // The key is the address; verify the bytes ARE their address. Public
    // objects are stored unencrypted, so nothing else attests to them — a
    // substituted or corrupted object would otherwise be cached and rendered
    // as genuine, and then kept.
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
    // `Vec::from` moves the `Bytes` allocation when it uniquely owns one, so the
    // plaintext is not copied on the way into the in-place encryption (#915).
    let encrypted = cache_encrypt(Vec::from(bytes), &db_key, content_hash.as_bytes())?;

    let dir = media_cache_dir()?;
    crate::private_fs::create_dir_all(&dir)
        .map_err(|e| Error::Other(anyhow::anyhow!("create cache dir: {e}")))?;

    // Atomic write, mirroring `get_media_url`: a half-written cache file would
    // be served as a corrupt image rather than re-fetched.
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

/// Where an upload's plaintext lives, and who is allowed to release it.
///
/// Both variants are read once and then let go before the R2 PUT (#915) — the
/// difference is what "let go" means. `Owned` bytes belong to this call and are
/// dropped; `Staged` bytes belong to `commands::staging`, which keeps them
/// until the upload has actually succeeded so a failed PUT is a retry rather
/// than a lost paste.
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

    /// Let go of this call's hold on the plaintext, so the longest step in the
    /// function — the PUT — does not run with both buffers resident.
    fn release(self) {
        drop(self);
    }
}

/// Upload a media file using convergent encryption.
///
/// Reads the file from the filesystem path (no bytes over IPC), so arbitrarily
/// large files work without memory or serialisation overhead. This is the path
/// for a file the USER already has on disk — a picker selection or an OS drag
/// and drop. Bytes that arrived through the webview have no path and go through
/// [`upload_media_staged`] instead; nothing in this crate writes a file in
/// order to have a path to pass here.
pub async fn upload_media(
    path: String,
    filename: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    // Read plaintext from disk.
    let data = tokio::fs::read(&path).await
        .map_err(|e| Error::Other(anyhow::anyhow!("read file {path}: {e}")))?;

    upload_plaintext(Plaintext::Owned(data), filename, content_type, state).await
}

/// Upload an attachment the renderer staged in memory (`commands::staging`) —
/// a paste, or a drop the webview surfaced as a `File` rather than a path.
///
/// The staged bytes are released here, and only on success: an upload that
/// fails leaves them staged so the user's paste survives a retry. See
/// `commands::staging` for why they are in memory rather than in a temp file.
pub async fn upload_media_staged(
    staged_id: String,
    filename: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    let bytes = crate::commands::staging::peek_staged(&staged_id).ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "no staged attachment {staged_id}: it was already uploaded, discarded, or the              session ended"
        ))
    })?;

    let result =
        upload_plaintext(Plaintext::Staged(bytes), filename, content_type, state).await;
    if result.is_ok() {
        crate::commands::staging::discard_staged(&staged_id);
    }
    result
}

/// The shared upload core.
///
/// Convergent encryption: SHA-256(plaintext) → deterministic AES-256-GCM key
/// via HKDF.  Same file uploaded by any user produces identical ciphertext →
/// identical R2 object → cross-user deduplication.
///
/// Dedup check against Turso's `attachment_object` table before uploading, so
/// the second upload of the same file by any user skips the R2 PUT entirely.
async fn upload_plaintext(
    data: Plaintext,
    filename: String,
    content_type: String,
    state: &Arc<AppState>,
) -> Result<MediaUploadResult> {
    let size_bytes = data.as_slice().len();

    // SHA-256 of plaintext — the dedup + key-derivation anchor.
    let hash_bytes = sha256_bytes(data.as_slice());
    let content_hash = hex::encode(hash_bytes);

    // Deterministic R2 key: same content → same path in R2.
    //
    // The original filename used to be appended here. It was decorative — the
    // content_hash is the uniqueness anchor — but it was also metadata the
    // operator could read at rest from the object listing alone, without
    // decrypting anything (#762). "budget-2026.pdf" is content, and this product
    // exists not to hold that. Dropped.
    //
    // Safe to change without a migration: the key is stored per object in
    // `attachment_object.r2_key` and download paths take it explicitly, so
    // objects written under the old format keep resolving under their stored
    // keys. Only new uploads take this shape.
    let r2_key = format!("media/{content_hash}.enc");
    let r2_url = format!("{}/{}", state.config.r2_endpoint.trim_end_matches('/'), r2_key);

    // Derive encryption key and nonce from the content hash (convergent).
    let (enc_key, enc_nonce) = derive_attachment_key(&hash_bytes);

    // Compute blurhash + dimensions before data is consumed.
    let (blurhash, width, height) = if content_type.starts_with("image/") {
        match compute_image_meta(data.as_slice()) {
            Ok((bh, w, h)) => (Some(bh), Some(w), Some(h)),
            Err(e) => {
                eprintln!("[upload_media] image meta failed for {filename}: {e}");
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    // Is this blob already stored? A content-addressed probe: the caller
    // computed the hash from bytes it already holds, so the question discloses
    // nothing it did not bring.
    let already_uploaded = crate::commands::ds_reads::object_exists(
        state,
        &content_hash,
        pollis_api::account_reads::ObjectKind::Attachment,
    )
    .await?;

    if !already_uploaded {
        // Encrypt with chunked AES-256-GCM, then upload via a DS-minted
        // presigned PUT (the client holds no R2 credentials).
        let ciphertext = encrypt_chunked(data.as_slice(), &enc_key, &enc_nonce);
        // Release the plaintext BEFORE the presign round trip and the upload
        // (#915). Everything derived from it — hash, key, blurhash, dimensions,
        // size — was computed above, and the PUT is the longest-lived step in
        // the function: holding both buffers across it doubled the resident cost
        // of an upload for the entire duration of the transfer.
        data.release();

        let put_url = presign_r2(state, "put", &r2_key).await?;
        let overlay = state.overlay_handle();
        r2_put_url(overlay.as_deref(), &put_url, ciphertext, "application/octet-stream").await?;

        // Register in Turso so future uploads of the same file skip R2 — route the
        // dedup-row write through the Delivery Service.
        // Upload-time dedup registration: no message carries this object yet,
        // so there is no reference to count. The send path registers the
        // `(content_hash, message_id)` reference separately (#690), and since
        // #925 it is a different VARIANT rather than the same body with a field
        // left `None`.
        let body = pollis_api::messages::AttachmentRegisterBody::ObjectOnly {
            content_hash: content_hash.clone(),
            r2_key: r2_key.clone(),
        };
        crate::commands::mls::ds_post_ok(state, &body).await?;
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
    let overlay = state.overlay_handle();
    let ciphertext = r2_get_url(overlay.as_deref(), &get_url).await?;
    let plaintext = decrypt_chunked(&ciphertext, &enc_key, &enc_nonce)?;
    // Explicit, not incidental (#915): without this the ciphertext stays alive
    // until the function returns, so the hash check below — and the caller's
    // first use of the result — run with two full-size copies resident.
    drop(ciphertext);

    // The object is CONTENT-ADDRESSED, so verify it actually is its address.
    //
    // AEAD alone proves nothing here: the key is DERIVED from `content_hash`
    // (`derive_attachment_key` above), and that hash is known to everyone the
    // attachment was shared with. Anyone holding it can therefore produce
    // ciphertext that decrypts cleanly — the tag only proves the writer knew the
    // hash, not that the bytes are the ones the sender meant. Re-hashing is what
    // closes that: a substituted payload has a different digest and is refused,
    // whoever managed to write it into R2.
    //
    // Cheap, and it also catches ordinary corruption in the shared convergent
    // object rather than rendering it as genuine.
    let actual = sha256_bytes(&plaintext);
    if actual != hash_array {
        return Err(Error::Other(anyhow::anyhow!(
            "attachment {r2_key} failed its content-hash check — expected {content_hash}, \
             got {}; refusing to return substituted or corrupted bytes",
            hex::encode(actual)
        )));
    }
    Ok(plaintext)
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
    if let Err(e) = crate::private_fs::create_dir_all(&dir) {
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
    // `bytes` is MOVED into the encryption (#915): it is the decrypted
    // attachment, and holding it alongside a freshly-allocated ciphertext was
    // the second of the three full-size copies this path used to keep alive.
    let encrypted = match cache_encrypt(bytes, &db_key, content_hash.as_bytes()) {
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
///
/// **Takes the plaintext by value and encrypts it in place (#915.)** The
/// previous signature took `&[u8]`, produced a fresh ciphertext buffer, then
/// copied nonce+ciphertext into a THIRD buffer — so caching a 100 MiB
/// attachment briefly held ~300 MiB: the decrypted bytes the caller still owned,
/// the `Vec` `encrypt` returned, and the concatenation. This reuses the caller's
/// allocation instead: AES-GCM in place, the 16-byte tag appended, and the
/// 12-byte nonce spliced onto the front by an in-place rotate.
///
/// On the real path that allocates NOTHING. `decrypt_chunked` sizes its output
/// from the CIPHERTEXT length, so the plaintext it returns already carries spare
/// capacity (one AEAD tag per chunk) — more than the 28 bytes needed here. A
/// caller whose buffer is exactly full pays one reallocation, i.e. one extra
/// copy in the worst case rather than two. Both bounds are asserted in
/// `tests/attachment_memory.rs` against a counting allocator.
///
/// `pollis-core` ships to mobile via uniffi, and this is the largest single
/// allocation the client makes — the difference between one copy and three is
/// the difference between caching a large video and being killed for it.
///
/// The on-disk layout is byte-for-byte unchanged, so existing cache files (and
/// [`cache_decrypt`]) are unaffected.
pub fn cache_encrypt(mut plaintext: Vec<u8>, db_key: &[u8], info: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::{aead::{AeadInPlace, KeyInit}, Aes256Gcm, Key, Nonce};
    use rand::RngCore;

    let key = derive_cache_key(db_key, info);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; CACHE_NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

    // Reserve the tag AND the nonce prefix up front, so neither the in-place
    // encryption nor the splice below can trigger a reallocation — a realloc
    // here would copy the whole buffer and put the second copy back.
    plaintext.reserve_exact(CACHE_NONCE_LEN + 16);
    cipher
        .encrypt_in_place(Nonce::from_slice(&nonce_bytes), &[], &mut plaintext)
        .map_err(|_| Error::Other(anyhow::anyhow!("media cache encrypt failed")))?;

    // Prepend the nonce within the existing allocation: extend by 12, then
    // rotate. `rotate_right` is an in-place memmove, not a second buffer.
    plaintext.extend_from_slice(&nonce_bytes);
    plaintext.rotate_right(CACHE_NONCE_LEN);
    Ok(plaintext)
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
    let overlay = state.overlay_handle();
    r2_delete_url(overlay.as_deref(), &delete_url).await
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
    presign_r2_with_length(state, operation, key, None).await
}

/// [`presign_r2`], optionally declaring the EXACT byte count a `put` will
/// carry. When present the DS signs `content-length` into the URL, so R2 itself
/// refuses a body of any other size.
///
/// Required for `emoji/…` puts (#848) — those objects are unencrypted, publicly
/// fetchable and hard-capped, and a cap the client merely honours is not a cap.
/// `None` reproduces the previous request byte for byte, which is why every
/// media/avatar call site is untouched.
pub(crate) async fn presign_r2_with_length(
    state: &Arc<AppState>,
    operation: &str,
    key: &str,
    content_length: Option<u64>,
) -> Result<String> {
    let body = pollis_api::broker::R2PresignBody {
        operation: operation.to_string(),
        key: key.to_string(),
        // The DS signs only `host`; the client sets Content-Type at upload time.
        content_type: None,
        content_length,
        // No-auth fallback for the acting user (`broker::resolve_user`): auth on
        // → taken from the verified signature and ignored here; auth off → this
        // IS the identity and a body without it is a 400. Presign has no
        // per-object authz, so this only ever satisfies the gate.
        user_id: Some(crate::commands::mls::current_user_id(state).await?),
    };
    let parsed = crate::commands::mls::ds_post_json(state, &body).await?;
    Ok(parsed.url)
}

/// PUT `data` to a presigned URL. Content-Type is set at request time (the broker
/// signs only `host`, so it is deliberately left unsigned). Routes through the
/// overlay when on — R2 is a first-party, allowlisted host (§14.2).
pub(crate) async fn r2_put_url(
    overlay: Option<&pollis_relay::OverlayHandle>,
    url: &str,
    data: Vec<u8>,
    content_type: &str,
) -> Result<()> {
    let resp = crate::net::overlay::http_client(overlay)
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

/// GET the bytes at a presigned URL. Routes through the overlay when on.
///
/// Returns [`bytes::Bytes`], not `Vec<u8>` (#915). `resp.bytes()` already owns
/// the whole body; the `.to_vec()` this replaces allocated a SECOND full-size
/// buffer and memcpy'd into it, so for a moment every download held two copies
/// of the attachment. `Bytes` derefs to `[u8]`, so every reader is unchanged,
/// and the one caller that genuinely needs an owned `Vec` converts at its own
/// boundary.
pub(crate) async fn r2_get_url(
    overlay: Option<&pollis_relay::OverlayHandle>,
    url: &str,
) -> Result<bytes::Bytes> {
    let resp = crate::net::overlay::http_client(overlay).get(url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("R2 download failed: {} — {}", status, body)));
    }
    Ok(resp.bytes().await?)
}

/// DELETE the object at a presigned URL. A 404 counts as success (already gone).
/// Routes through the overlay when on.
async fn r2_delete_url(overlay: Option<&pollis_relay::OverlayHandle>, url: &str) -> Result<()> {
    let resp = crate::net::overlay::http_client(overlay).delete(url).send().await?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 404 {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    Err(Error::Other(anyhow::anyhow!("R2 delete failed: {} — {}", status, body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// `CACHE_DIR_WALKS` is process-global, so every test that sweeps a cache
    /// directory has to hold this — not just the one that reads the counter,
    /// or a concurrent sweep inflates its delta.
    static WALK_COUNTER: Mutex<()> = Mutex::new(());

    fn serialise_sweeps() -> std::sync::MutexGuard<'static, ()> {
        WALK_COUNTER.lock().unwrap_or_else(|e| e.into_inner())
    }

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    /// A private directory for one test. Not `tempfile` — pollis-core does not
    /// depend on it, and one `create_dir_all` is the whole requirement.
    fn scratch_dir(tag: &str) -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pollis-r2-{tag}-{}-{n}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Write `bytes` bytes to `<dir>/<name>`, then stamp its mtime so
    /// "oldest first" is deterministic rather than dependent on filesystem
    /// timestamp granularity.
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

    /// The cap is a real eviction, oldest first, and it leaves the newest
    /// entries alone. This is the behaviour that has to keep working now that
    /// the only thing driving it is a cache write (#930).
    #[test]
    fn the_cap_evicts_oldest_first_and_stops_at_the_target() {
        let _serial = serialise_sweeps();
        let dir = scratch_dir("evict");
        // 400 bytes total, oldest first: a=100 (oldest), b=100, c=200 (newest).
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

    /// In-progress writes are not cache entries and must survive a sweep.
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

    /// The counter the focus-path guard below is asserted against actually
    /// counts. Without this, a guard reading "0 walks" would also pass if the
    /// counter were broken and never incremented at all.
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

    /// GUARD (#930): window focus must not walk the media cache.
    ///
    /// A source scan, because the thing being asserted is a rule about which
    /// code may run on a UI event, and the offending call lived in a Tauri
    /// event closure that no test can dispatch to. Paired with the counter test
    /// above, which is what makes "does not walk" mean something: the sweep is
    /// instrumented, so if it ever returns to the focus arm the reviewer of
    /// that change has to delete this test to land it.
    #[test]
    fn guard_window_focus_does_not_sweep_the_media_cache() {
        let shell = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../src-tauri/src/lib.rs"));

        let focus_arm_start = shell
            .find("tauri::WindowEvent::Focused(true)")
            .expect("the shell must still have a window-focus arm for this guard to mean anything");
        // The focus arm runs to the next `WindowEvent` match arm; scanning to
        // the end of the closure is enough and cannot miss a call inside it.
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
        // Only the module itself, not this test module — the assertion message
        // below names the symbol, and a whole-file scan would match its own
        // text and fail forever.
        let source = include_str!("r2.rs");
        let module = &source[..source.find("mod tests {").unwrap_or(source.len())];
        // Assembled rather than written out, for the same reason.
        let banned = format!("pub fn {}", "enforce_cache_cap_now");
        assert!(
            !module.contains(&banned),
            "`enforce_cache_cap_now` existed only to be called from window focus (#930); \
             re-exporting it re-opens that door — the cap belongs to the write path"
        );
    }

    // ── The media-cache wipe (#994) ───────────────────────────────────────

    /// Put one cache file in `<root>/<user>/` and return its path.
    fn seed_cache_entry(user: &str, name: &str) -> PathBuf {
        let dir = test_cache_root().join(user);
        crate::private_fs::create_dir_all(&dir).expect("create user cache dir");
        let path = dir.join(name);
        std::fs::write(&path, b"decrypted-media-bytes").expect("seed cache entry");
        path
    }

    /// The property the wipe never actually had: a file that was in the cache
    /// beforehand is not there afterwards.
    ///
    /// It failed on every caller because the directory was resolved from
    /// ambient state — `CURRENT_CACHE_USER`, which `unload_user_db` had already
    /// cleared by the time logout wiped, and which the PIN paths had not yet
    /// set. `_anon/` was empty, so the wipe reported nothing and removed
    /// nothing. Naming the user is what fixes it, so this test names one and
    /// deliberately points the ambient user somewhere else first.
    #[test]
    fn wiping_one_user_removes_that_user_s_files() {
        let _serial = serialise_sweeps();
        let entry = seed_cache_entry("wipe-user-a", "aaaa.png.enc");
        assert!(entry.exists());

        // The ambient user is wrong — as it is at every real call site.
        set_cache_user(None);

        clear_media_cache(CacheScope::User("wipe-user-a"));

        assert!(
            !entry.exists(),
            "the wipe left the user's cached media on disk"
        );
    }

    /// And it removes only that user's files: a second client signed in as
    /// somebody else on the same machine keeps its cache.
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

    /// `Everything` reaches into the per-user subdirectories. The root's own
    /// entries are DIRECTORIES, so the old `remove_file`-per-entry loop would
    /// have deleted nothing at all here.
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
}
