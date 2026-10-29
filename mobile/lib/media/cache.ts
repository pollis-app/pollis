// Mobile media transport (#346). iOS ATS and Android cleartext rules block
// desktop's loopback media server, so Rust decrypts to a file in the sandbox
// cache dir and `expo-image` loads the `file://` URI. Files are ref-counted and
// unlinked on the last release, so the decrypted plaintext never outlives the
// render.

import * as FileSystem from "expo-file-system/legacy";
import { invoke } from "../native";
import { exportFilename } from "./viewer";
import type { AttachmentKeyScope, MessageAttachment } from "../../types";

// A dedicated subfolder so our teardown can't touch anything else.
export const MEDIA_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-media/`;

interface GetMediaPathArgs {
  r2Key: string;
  contentHash: string;
  // The digest the decrypted bytes must match, and which key sealed them (#1382).
  sha256: string;
  keyScope: AttachmentKeyScope;
  contentType: string;
  // Passed from JS so the platform layer owns the sandbox path.
  destDir: string;
}

const inFlight = new Map<string, Promise<string>>();

// Live consumers per content hash; the file is unlinked at zero.
const refCounts = new Map<string, number>();

const resolvedUris = new Map<string, string>();

let dirEnsured: Promise<void> | null = null;

// Bumped by every `clearMediaCache()`. A resolve still in flight from the ended
// session must not register its file, and a late release must not decrement a
// count the next session now owns.
let generation = 0;

// Hand this back to `releaseMediaUri` so a release that outlives a clear is a no-op.
export function mediaCacheGeneration(): number {
  return generation;
}

function ensureDir(): Promise<void> {
  if (dirEnsured) {
    return dirEnsured;
  }
  const pending = FileSystem.makeDirectoryAsync(MEDIA_DIR, {
    intermediates: true,
  }).catch((err: unknown) => {
    // Reset so a transient failure can retry on the next resolve.
    dirEnsured = null;
    throw err;
  });
  dirEnsured = pending;
  return pending;
}

// Resolve an attachment to a `file://` URI of its decrypted bytes. Takes a
// reference: pair every successful call with `releaseMediaUri()`.
export async function resolveMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "sha256" | "key_scope"
  >,
): Promise<string> {
  const { object_key, content_hash, content_type, sha256, key_scope } = attachment;

  // Optimistic sends have no object key yet (use `localPreviewUri`), and a
  // reference with no key scope (pre-#1382) cannot be opened.
  if (!object_key || !content_hash || !key_scope) {
    throw new Error("resolveMediaUri: attachment is not available to fetch");
  }

  const existing = inFlight.get(content_hash);
  if (existing) {
    refCounts.set(content_hash, (refCounts.get(content_hash) ?? 0) + 1);
    return existing;
  }

  const startedIn = generation;
  const promise = (async () => {
    await ensureDir();
    const uri = await invoke<string>("get_media_path", {
      r2Key: object_key,
      contentHash: content_hash,
      sha256,
      keyScope: key_scope,
      contentType: content_type,
      destDir: MEDIA_DIR,
    } satisfies GetMediaPathArgs);
    if (!uri) {
      throw new Error(`get_media_path returned empty path for ${content_hash}`);
    }
    if (startedIn !== generation) {
      // Cleared (sign-out) while decrypting: don't hand the ended session's
      // plaintext to anyone.
      await FileSystem.deleteAsync(uri, { idempotent: true });
      throw new Error(`media cache cleared while resolving ${content_hash}`);
    }
    resolvedUris.set(content_hash, uri);
    return uri;
  })();

  inFlight.set(content_hash, promise);
  refCounts.set(content_hash, (refCounts.get(content_hash) ?? 0) + 1);

  promise.catch(() => {
    // Let the next consumer retry; the consumer's release reconciles the count.
    // Only drop our own entry: after a clear the slot may hold a newer resolve.
    if (inFlight.get(content_hash) === promise) {
      inFlight.delete(content_hash);
    }
  });

  return promise;
}

// Release one reference; the last one unlinks the file. Safe after a failed
// resolve. Pass the `mediaCacheGeneration()` captured at resolve time.
export async function releaseMediaUri(
  contentHash: string,
  resolvedIn?: number,
): Promise<void> {
  if (resolvedIn !== undefined && resolvedIn !== generation) {
    return;
  }
  const next = (refCounts.get(contentHash) ?? 0) - 1;
  if (next > 0) {
    refCounts.set(contentHash, next);
    return;
  }

  refCounts.delete(contentHash);
  inFlight.delete(contentHash);
  const uri = resolvedUris.get(contentHash);
  resolvedUris.delete(contentHash);
  if (!uri) {
    return;
  }
  await FileSystem.deleteAsync(uri, { idempotent: true });
}

// Named copies (#1248): the cache file has no extension, but AVPlayer, the iOS
// photo library and the share sheet type a file by its extension (and the share
// sheet shows the name). So the viewer takes a second, ref-counted local copy
// under MEDIA_DIR/named/<hash>/ carrying the sender's filename.

const NAMED_DIR = `${MEDIA_DIR}named/`;
const namedInFlight = new Map<string, Promise<string>>();
const namedRefCounts = new Map<string, number>();
// Base references held per hash: one per resolve attempt, so a retried failure
// releases them all.
const namedBaseRefs = new Map<string, number>();

/**
 * Resolve an attachment to a decrypted copy carrying its filename and
 * extension. Pair every call with `releaseNamedMediaUri`.
 */
export async function resolveNamedMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "filename" | "localPreviewUri" | "sha256" | "key_scope"
  >,
): Promise<string> {
  const { object_key, content_hash, content_type, filename } = attachment;
  if (!object_key && attachment.localPreviewUri) {
    return attachment.localPreviewUri;
  }
  if (!object_key || !content_hash) {
    throw new Error("resolveNamedMediaUri: attachment has no object_key/content_hash");
  }

  namedRefCounts.set(content_hash, (namedRefCounts.get(content_hash) ?? 0) + 1);
  const existing = namedInFlight.get(content_hash);
  if (existing) {
    return existing;
  }

  const startedIn = generation;
  const promise = (async () => {
    namedBaseRefs.set(content_hash, (namedBaseRefs.get(content_hash) ?? 0) + 1);
    const source = await resolveMediaUri(attachment);
    const dir = `${NAMED_DIR}${content_hash}/`;
    await FileSystem.makeDirectoryAsync(dir, { intermediates: true }).catch(() => {
      // Already there from an earlier copy of the same bytes.
    });
    const target = `${dir}${exportFilename(filename, content_type, content_hash)}`;
    await FileSystem.deleteAsync(target, { idempotent: true });
    await FileSystem.copyAsync({ from: source, to: target });
    if (startedIn !== generation) {
      // Cleared (sign-out) while copying: this copy landed after the wipe.
      await FileSystem.deleteAsync(dir, { idempotent: true });
      throw new Error(`media cache cleared while copying ${content_hash}`);
    }
    return target;
  })();
  namedInFlight.set(content_hash, promise);
  promise.catch(() => {
    // Let the next caller retry; only drop our own entry.
    if (namedInFlight.get(content_hash) === promise) {
      namedInFlight.delete(content_hash);
    }
  });
  return promise;
}

export async function releaseNamedMediaUri(
  attachment: Pick<MessageAttachment, "object_key" | "content_hash">,
  resolvedIn?: number,
): Promise<void> {
  const { object_key, content_hash } = attachment;
  if (!object_key || !content_hash) {
    return;
  }
  if (resolvedIn !== undefined && resolvedIn !== generation) {
    return;
  }
  const next = (namedRefCounts.get(content_hash) ?? 0) - 1;
  if (next > 0) {
    namedRefCounts.set(content_hash, next);
    return;
  }
  namedRefCounts.delete(content_hash);
  namedInFlight.delete(content_hash);
  const baseRefs = namedBaseRefs.get(content_hash) ?? 0;
  namedBaseRefs.delete(content_hash);
  await FileSystem.deleteAsync(`${NAMED_DIR}${content_hash}/`, { idempotent: true });
  for (let i = 0; i < baseRefs; i++) {
    await releaseMediaUri(content_hash, generation);
  }
}

// Sign-out teardown, called from `endSession()` (lib/session).
export async function clearMediaCache(): Promise<void> {
  generation += 1;
  inFlight.clear();
  refCounts.clear();
  resolvedUris.clear();
  namedInFlight.clear();
  namedRefCounts.clear();
  namedBaseRefs.clear();
  dirEnsured = null;
  await FileSystem.deleteAsync(MEDIA_DIR, { idempotent: true });
}
