// Mobile media transport (#346): Rust decrypts to a sandbox file; ref-counted, and unlinked once
// released unless it is among the few most recently released (kept until lock or leaving the app).

import { AppState } from "react-native";
import { reaction } from "mobx";
import * as FileSystem from "expo-file-system/legacy";
import { invoke } from "../native";
import { Image } from "expo-image";
import { exportFilename } from "./viewer";
import { createMediaStore } from "./store";
import { appStore } from "../../stores/appStore";
import type { AttachmentKeyScope, MessageAttachment } from "../../types";

// A dedicated subfolder of OS-evictable cache storage.
export const MEDIA_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-media/`;

interface GetMediaPathArgs {
  r2Key: string;
  contentHash: string;
  // The digest the decrypted bytes must match, and which key sealed them (#1382).
  sha256: string;
  keyScope: AttachmentKeyScope;
  contentType: string;
  // Absolute sandbox dir Rust decrypts into; Expo owns the path.
  destDir: string;
}

// Ref-counted decrypted files, plus a few just-released ones kept so a remount draws at once (./store.ts).
export const MEDIA_RETAIN_MAX = 32;
const store = createMediaStore({
  unlink: (uri) => FileSystem.deleteAsync(uri, { idempotent: true }),
  retainMax: MEDIA_RETAIN_MAX,
});

let dirEnsured: Promise<void> | null = null;

export function mediaCacheGeneration(): number {
  return store.generation();
}

function ensureDir(): Promise<void> {
  if (dirEnsured) {
    return dirEnsured;
  }
  const pending = FileSystem.makeDirectoryAsync(MEDIA_DIR, {
    intermediates: true,
  }).catch((err: unknown) => {
    // Reset so a transient failure can retry.
    dirEnsured = null;
    throw err;
  });
  dirEnsured = pending;
  return pending;
}

/** The decrypted file already on disk for `contentHash`, synchronously, so a remount skips the placeholder. */
export function peekMediaUri(contentHash: string | null | undefined): string | null {
  return contentHash ? store.peek(contentHash) : null;
}

/** Called whenever any consumer's resolve of `contentHash` succeeds (#1826); returns the unsubscribe. */
export function subscribeMediaResolved(contentHash: string, listener: () => void): () => void {
  return store.subscribe(contentHash, listener);
}

// Every successful resolve must be paired with a `releaseMediaUri()`.
export async function resolveMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "sha256" | "key_scope"
  >,
): Promise<string> {
  const { object_key, content_hash, content_type, sha256, key_scope } = attachment;

  // No object key (optimistic send) or no key scope (pre-#1382): never reaches Rust.
  if (!object_key || !content_hash || !key_scope) {
    throw new Error("resolveMediaUri: attachment is not available to fetch");
  }

  return store.acquire(content_hash, async () => {
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
    return uri;
  });
}

// Pass the `mediaCacheGeneration()` captured at resolve: a pre-clear release is a no-op.
export async function releaseMediaUri(
  contentHash: string,
  resolvedIn?: number,
): Promise<void> {
  await store.release(contentHash, resolvedIn);
}

/** Lock and leaving the app: unlink every decrypted file nothing is drawing, and drop decoded copies. */
export async function trimMediaCache(): Promise<void> {
  Image.clearMemoryCache().catch(() => undefined);
  await store.trim();
}

let retentionHooked = false;

/** Kept files live only while the app is in the foreground and unlocked. Once per process. */
export function startMediaRetention(): void {
  if (retentionHooked) {
    return;
  }
  retentionHooked = true;
  AppState.addEventListener("change", (state) => {
    if (state === "background") {
      void trimMediaCache();
    }
  });
  reaction(
    () => appStore.isLocked,
    (locked) => {
      if (locked) {
        void trimMediaCache();
      }
    },
  );
}

// Named copies (#1248): AVPlayer, the photo library and the share sheet type a file by its extension.

const NAMED_DIR = `${MEDIA_DIR}named/`;
const namedInFlight = new Map<string, Promise<string>>();
const namedRefCounts = new Map<string, number>();
const namedBaseRefs = new Map<string, number>();

/** Resolve to a decrypted copy that carries its filename; pair with `releaseNamedMediaUri`. */
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

  const startedIn = store.generation();
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
    if (startedIn !== store.generation()) {
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

/** As `releaseMediaUri`: pass the generation captured at resolve. */
export async function releaseNamedMediaUri(
  attachment: Pick<MessageAttachment, "object_key" | "content_hash">,
  resolvedIn?: number,
): Promise<void> {
  const { object_key, content_hash } = attachment;
  if (!object_key || !content_hash) {
    return;
  }
  if (resolvedIn !== undefined && resolvedIn !== store.generation()) {
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
    await releaseMediaUri(content_hash, store.generation());
  }
}

// Sign-out teardown; called from `endSession()` (lib/session).
export async function clearMediaCache(): Promise<void> {
  store.clear();
  Image.clearMemoryCache().catch(() => undefined);
  namedInFlight.clear();
  namedRefCounts.clear();
  namedBaseRefs.clear();
  dirEnsured = null;
  await FileSystem.deleteAsync(MEDIA_DIR, { idempotent: true });
}
