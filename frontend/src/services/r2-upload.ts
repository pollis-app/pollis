import { invoke } from '../bridge';
import type { MessageAttachment, PresignedUploadResponse } from '../types';
import { IMAGE_EXT_MIME } from '../utils/fileIcon';

export async function uploadAvatar(
  userId: string,
  _aliasId: string,
  file: File,
): Promise<PresignedUploadResponse> {
  const data = new Uint8Array(await file.arrayBuffer());
  // Content-addressed (#874): Rust writes `avatars/{userId}/{sha256}.{ext}`, so
  // a new avatar is a new key and cached copies never go stale.
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

// One live blob URL per legacy key; the previous one is revoked on replace.
const legacyBlobUrls = new Map<string, string>();

/// Resolve a public object (avatar, group icon) to an `<img src>`: a loopback
/// media-server URL backed by the disk cache, or, on the empty-string
/// sentinel (a pre-#874 legacy key, or no media server), a Blob URL with the
/// MIME from the extension or magic bytes.
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

/// The object, digest and sealing key (#1382) for media commands. Throws when
/// there is no key scope (uploading, or pre-#1382), so Rust never guesses a key.
export function mediaArgs(attachment: MessageAttachment) {
  if (!attachment.key_scope) {
    throw new Error('attachment is unavailable');
  }
  return {
    r2Key: attachment.object_key,
    contentHash: attachment.content_hash,
    sha256: attachment.sha256,
    keyScope: attachment.key_scope,
  };
}

/// Download and decrypt an attachment into a blob URL.
export async function downloadAndDecryptMedia(attachment: MessageAttachment): Promise<string> {
  const bytes = await invoke<number[]>('download_media', mediaArgs(attachment));
  const mimeType = attachment.content_type;
  const blob = new Blob([new Uint8Array(bytes)], mimeType ? { type: mimeType } : undefined);
  return URL.createObjectURL(blob);
}

// In-flight de-dup per hash, so mounted rows don't fire identical invokes;
// resolved URLs stay cached (an object id names one immutable object).
const inFlight = new Map<string, Promise<string>>();

/// An attachment URL for `<img>` / `<audio>` / `<video>`: Rust caches it
/// re-encrypted on disk and returns `http://127.0.0.1:<port>/<token>/<hash>`
/// (`pollis-core::media_server`). Files over the 100 MiB cap get an empty
/// string and fall back to `downloadAndDecryptMedia`.
export async function getMediaUrl(attachment: MessageAttachment): Promise<string> {
  const contentHash = attachment.content_hash;
  const cached = inFlight.get(contentHash);
  if (cached) {
    return cached;
  }
  const promise = (async () => {
    const url = await invoke<string>('get_media_url', {
      ...mediaArgs(attachment),
      contentType: attachment.content_type,
    });
    if (!url) {
      return downloadAndDecryptMedia(attachment);
    }
    return url;
  })();
  inFlight.set(contentHash, promise);
  promise.catch(() => {
    inFlight.delete(contentHash);
  });
  return promise;
}
