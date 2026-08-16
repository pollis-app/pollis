import { invoke } from '../bridge';
import type { PresignedUploadResponse } from '../types';
import { IMAGE_EXT_MIME } from '../utils/fileIcon';

export async function uploadAvatar(
  userId: string,
  _aliasId: string,
  file: File,
): Promise<PresignedUploadResponse> {
  const data = new Uint8Array(await file.arrayBuffer());
  // Content-addressed: Rust hashes the bytes and writes
  // `avatars/{userId}/{sha256}.{ext}` (#874). The key used to be a bare
  // `avatars/{userId}` overwritten in place, and a mutable key is what made
  // every viewer re-download every avatar on every launch — there was no way
  // to tell a cached copy was still current. A new avatar is now a new key,
  // which the profile row already publishes.
  const result = await invoke<{ key: string; url: string }>('upload_public_file', {
    prefix: `avatars/${userId}`,
    data: Array.from(data),
    contentType: file.type || 'image/png',
  });
  return { upload_url: '', object_key: result.key, public_url: result.url };
}

export async function uploadGroupIcon(
  groupId: string,
  file: File,
): Promise<PresignedUploadResponse> {
  const data = new Uint8Array(await file.arrayBuffer());
  const result = await invoke<{ key: string; url: string }>('upload_public_file', {
    prefix: `group-icons/${groupId}`,
    data: Array.from(data),
    contentType: file.type || 'image/png',
  });
  return { upload_url: '', object_key: result.key, public_url: result.url };
}

function sniffImageMime(bytes: Uint8Array): string | null {
  if (bytes.length >= 8 &&
    bytes[0] === 0x89 && bytes[1] === 0x50 && bytes[2] === 0x4e && bytes[3] === 0x47) {
    return 'image/png';
  }
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff) {
    return 'image/jpeg';
  }
  if (bytes.length >= 6 &&
    bytes[0] === 0x47 && bytes[1] === 0x49 && bytes[2] === 0x46 && bytes[3] === 0x38) {
    return 'image/gif';
  }
  if (bytes.length >= 12 &&
    bytes[0] === 0x52 && bytes[1] === 0x49 && bytes[2] === 0x46 && bytes[3] === 0x46 &&
    bytes[8] === 0x57 && bytes[9] === 0x45 && bytes[10] === 0x42 && bytes[11] === 0x50) {
    return 'image/webp';
  }
  return null;
}

// One live blob URL per legacy key. `URL.createObjectURL` pins its Blob in
// memory until revoked, and this used to revoke nothing at all — every refetch
// of the same avatar leaked another copy of the image for the life of the
// document. Revoking the previous URL for a key when a new one replaces it
// bounds that to one per distinct key.
const legacyBlobUrls = new Map<string, string>();

/// Resolve a public (unencrypted) object — an avatar or group icon — to a URL
/// the webview can use as `<img src>`.
///
/// Primary path: `get_public_file_url` returns a loopback media-server URL
/// (`http://127.0.0.1:<port>/<token>/<hash>`) backed by the on-disk cache, so
/// the bytes never cross the JSON IPC and a restart re-reads them from disk
/// instead of R2. Same mechanism attachments and custom emoji already use.
///
/// Fallback, when Rust hands back the empty-string sentinel: a LEGACY key
/// written before objects were content-addressed (`avatars/{userId}`), or a
/// media server that isn't up. Those come down as bytes and become an
/// in-memory Blob URL, exactly as every avatar did before #874. The MIME type
/// is taken from the key extension when present and sniffed from magic bytes
/// otherwise, so GIFs and animated WebPs still animate.
export async function getFileDownloadUrl(key: string): Promise<string> {
  const served = await invoke<string>('get_public_file_url', { key });
  if (served) {
    return served;
  }

  const raw = await invoke<number[]>('download_file', { key });
  const bytes = new Uint8Array(raw);
  const ext = key.split('.').pop()?.toLowerCase() ?? '';
  const mimeType = IMAGE_EXT_MIME[ext] ?? sniffImageMime(bytes) ?? 'image/png';
  const blob = new Blob([bytes], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const previous = legacyBlobUrls.get(key);
  if (previous) {
    URL.revokeObjectURL(previous);
  }
  legacyBlobUrls.set(key, url);
  return url;
}

/// Download an encrypted media attachment, decrypt it, and return a blob URL
/// safe for use as <img src> or an anchor href.
/// The content_hash is used to derive the AES-256-GCM key via HKDF on the
/// Rust side — no key material is stored in the message or on the server.
export async function downloadAndDecryptMedia(
  r2Key: string,
  contentHash: string,
  mimeType?: string,
): Promise<string> {
  const bytes = await invoke<number[]>('download_media', { r2Key, contentHash });
  const blob = new Blob([new Uint8Array(bytes)], mimeType ? { type: mimeType } : undefined);
  return URL.createObjectURL(blob);
}

// In-flight de-dup: while one caller is resolving a URL for a hash, any
// other caller for the same hash awaits the same promise. Prevents a
// render storm during scroll where 30 mounted MessageItems each kick
// off identical invokes. Resolved promises stay cached for the life of
// the document — content_hash is content-addressed, so the URL is
// permanently correct (the loopback server's port + token live for the
// process; an unlock event would invalidate cached blob URLs from the
// over-cap fallback path, but that's a fresh document anyway).
const inFlight = new Map<string, Promise<string>>();

/// Resolve an attachment to a URL the webview can render directly via
/// `<img src>` / `<audio src>` / `<video src>`.
///
/// Default path: the Rust side downloads, decrypts, re-encrypts under
/// the per-session cache key, writes to disk, and returns
/// `http://127.0.0.1:<port>/<token>/<hash>`. Subsequent calls for the
/// same hash return the URL straight from the cached path. The local
/// HTTP server (see `pollis-core::media_server`) decrypts and streams
/// bytes — never through the JSON IPC, never via `Blob`/`asset://`.
///
/// Fallback: files larger than the per-file cap (100 MiB) skip the
/// disk cache; Rust returns an empty string, and we fall back to
/// `downloadAndDecryptMedia` which produces an in-memory Blob URL just
/// for this render.
export async function getMediaUrl(
  r2Key: string,
  contentHash: string,
  contentType: string,
): Promise<string> {
  const cached = inFlight.get(contentHash);
  if (cached) {
    return cached;
  }
  const promise = (async () => {
    const url = await invoke<string>('get_media_url', {
      r2Key,
      contentHash,
      contentType,
    });
    if (!url) {
      return downloadAndDecryptMedia(r2Key, contentHash, contentType);
    }
    return url;
  })();
  inFlight.set(contentHash, promise);
  promise.catch(() => {
    // On error, drop the cached rejection so the next caller retries.
    inFlight.delete(contentHash);
  });
  return promise;
}
