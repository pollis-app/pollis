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
  const key = `avatars/${userId}/${Date.now()}-${sanitizeFilename(file.name)}`;
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

/// Download a public (unencrypted) file from R2 and return a blob URL.
/// Used for avatars and group icons uploaded via upload_file.
/// The MIME type is derived from the key extension so the browser correctly
/// identifies GIFs and animated WebPs and plays them in <img> elements.
export async function getFileDownloadUrl(key: string): Promise<string> {
  const bytes = await invoke<number[]>('download_file', { key });
  const ext = key.split('.').pop()?.toLowerCase() ?? '';
  const mimeType = EXT_MIME[ext] ?? 'image/png';
  const blob = new Blob([new Uint8Array(bytes)], { type: mimeType });
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
