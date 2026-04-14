import { invoke } from '@tauri-apps/api/core';
import type { PresignedUploadResponse } from '../types';

function sanitizeFilename(name: string): string {
  return name.replace(/[^A-Za-z0-9._-]/g, '_');
}

export async function uploadAvatar(
  userId: string,
  _aliasId: string,
  file: File,
): Promise<PresignedUploadResponse> {
  const data = new Uint8Array(await file.arrayBuffer());
  // Stable per-user key so each upload overwrites the previous object in R2.
  // No extension or timestamp — the content-type is stored on the R2 object
  // at PUT time, and the frontend sniffs magic bytes on download to pick a
  // MIME type for the Blob.
  const key = `avatars/${userId}`;
  const result = await invoke<{ key: string; url: string }>('upload_file', {
    key,
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
  const key = `group-icons/${groupId}/${Date.now()}-${sanitizeFilename(file.name)}`;
  const result = await invoke<{ key: string; url: string }>('upload_file', {
    key,
    data: Array.from(data),
    contentType: file.type || 'image/png',
  });
  return { upload_url: '', object_key: result.key, public_url: result.url };
}

const EXT_MIME: Record<string, string> = {
  gif: 'image/gif',
  webp: 'image/webp',
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  svg: 'image/svg+xml',
  avif: 'image/avif',
};

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

/// Download a public (unencrypted) file from R2 and return a blob URL.
/// Used for avatars and group icons uploaded via upload_file.
/// The MIME type is derived from the key extension when present; for keys
/// without a known extension (e.g. stable avatar keys `avatars/{userId}`),
/// we sniff the file's magic bytes so the browser correctly identifies
/// GIFs and animated WebPs and plays them in <img> elements.
export async function getFileDownloadUrl(key: string): Promise<string> {
  const raw = await invoke<number[]>('download_file', { key });
  const bytes = new Uint8Array(raw);
  const ext = key.split('.').pop()?.toLowerCase() ?? '';
  const mimeType = EXT_MIME[ext] ?? sniffImageMime(bytes) ?? 'image/png';
  const blob = new Blob([bytes], { type: mimeType });
  return URL.createObjectURL(blob);
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
