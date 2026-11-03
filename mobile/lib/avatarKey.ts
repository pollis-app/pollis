// Profile picture keys and the renditions core writes for them (pollis-core/src/commands/avatar.rs). Pure.

// Must equal AVATAR_RENDITIONS in avatar.rs; tests/avatars.test.ts compares them.
export const AVATAR_RENDITIONS = [128, 256, 512] as const;

export type Rendition = (typeof AVATAR_RENDITIONS)[number];

const HEX64 = /^[0-9a-f]{64}$/;
const USER_ID = /^[A-Za-z0-9-]{1,128}$/;

/** A user id core will put in a file name. */
export function isFileUserId(id: string | null | undefined): id is string {
  return !!id && USER_ID.test(id);
}

/**
 * The content hash of `avatars/<user>/<sha256>.<ext>`, or null for "no picture":
 * an empty value (removed), a missing one, or a legacy mutable key.
 */
export function avatarHash(key: string | null | undefined): string | null {
  if (!key) {
    return null;
  }
  const parts = key.split("/");
  if (parts.length !== 3 || parts[0] !== "avatars" || !isFileUserId(parts[1])) {
    return null;
  }
  const dot = parts[2].lastIndexOf(".");
  if (dot < 0) {
    return null;
  }
  const hash = parts[2].slice(0, dot);
  const ext = parts[2].slice(dot + 1);
  if (!HEX64.test(hash) || !["png", "jpg", "jpeg", "gif", "webp"].includes(ext)) {
    return null;
  }
  return hash;
}

/** The smallest rendition at least `px` wide; the largest when none is. */
export function renditionFor(px: number): Rendition {
  for (const r of AVATAR_RENDITIONS) {
    if (r >= px) {
      return r;
    }
  }
  return AVATAR_RENDITIONS[AVATAR_RENDITIONS.length - 1];
}

export interface RenditionFile {
  userId: string;
  hash: string;
  px: number;
  name: string;
}

/** `<user>_<sha256>_<px>.<jpg|png>` → its parts; null for anything else (temp files included). */
export function parseRenditionFile(name: string): RenditionFile | null {
  const m = /^([A-Za-z0-9-]{1,128})_([0-9a-f]{64})_(\d{2,4})\.(jpg|png)$/.exec(name);
  if (!m) {
    return null;
  }
  return { userId: m[1], hash: m[2], px: Number(m[3]), name };
}

/** The same picture at another size: core writes every rendition with one extension. */
export function siblingUri(uri: string, px: number): string {
  return uri.replace(/_(\d{2,4})\.(jpg|png)$/, `_${px}.$2`);
}
