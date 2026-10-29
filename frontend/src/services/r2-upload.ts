import { invoke } from '../bridge';
import type { MessageAttachment, PresignedUploadResponse } from '../types';
import { IMAGE_EXT_MIME } from '../utils/fileIcon';

export async function uploadAvatar(
  userId: string,
  _aliasId: string,
  file: File,
): Promise<PresignedUploadResponse> {
  const data = new Uint8Array(await file.arrayBuffer());
  // Content-addressed `avatars/{userId}/{sha256}.{ext}` (#874), so caches stay valid.
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

// One live blob URL per legacy key; the previous one is revoked.
const legacyBlobUrls = new Map<string, string>();

/// Resolve an avatar or group icon to a loopback media URL; legacy keys fall back to a sniffed Blob URL (#874).
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

/// Media command args (#1382); throws without a key scope so nothing is decrypted under a guess.
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

/// Download + decrypt an attachment into a blob URL.
export async function downloadAndDecryptMedia(attachment: MessageAttachment): Promise<string> {
  const bytes = await invoke<number[]>('download_media', mediaArgs(attachment));
  const mimeType = attachment.content_type;
  const blob = new Blob([new Uint8Array(bytes)], mimeType ? { type: mimeType } : undefined);
  return URL.createObjectURL(blob);
}

// De-dup per object id; resolved URLs stay valid for the document.
const inFlight = new Map<string, Promise<string>>();

/// Attachment → loopback media-server URL; over the 100 MiB cap, a Blob URL via `downloadAndDecryptMedia`.
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
