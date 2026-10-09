// Mobile media transport — the file-path approach (issue #346).
//
// On desktop, `pollis-core` serves decrypted media over a loopback HTTP
// server and the renderer uses `<img src="http://127.0.0.1:…">`. That
// transport is blocked on iOS (App Transport Security) and Android
// (cleartext-traffic restrictions), so mobile takes the other supported
// route: Rust decrypts the R2-cached bytes to a file under the app's
// sandbox cache dir and returns the path; the renderer points
// `expo-image` at the resulting `file://` URI.
//
// This module owns the JS half of that contract:
//
//   - `resolveMediaUri(attachment)` invokes the Rust `get_media_path`
//     command (analogous to desktop's `get_media_url`) and returns a
//     `file://` URI. Concurrent callers for the same content hash share
//     one in-flight promise — the desktop `getMediaUrl` dedup pattern.
//   - Reference counting + `releaseMediaUri()` implement the issue's
//     "unlink-on-unmount so the plaintext path never outlives the
//     render" recommendation. The last release unlinks the file so
//     decrypted plaintext doesn't linger in the cache dir.
//
// Nothing here knows whether the bytes came from a real Rust decrypt or
// a dev mock (see ./mock.ts) — it only talks to the `invoke()` seam, so
// the whole pipeline can be exercised before the Rust `get_media_path`
// command lands.

import * as FileSystem from "expo-file-system/legacy";
import { invoke } from "../native";
import { exportFilename } from "./viewer";
import type { MessageAttachment } from "../../types";

// Decrypted media lives under the app sandbox cache dir, in a dedicated
// subfolder so a cache wipe (or our own teardown) can't touch anything
// else. `cacheDirectory` is OS-evictable storage — appropriate for
// content-addressed, re-fetchable media.
export const MEDIA_DIR = `${FileSystem.cacheDirectory ?? ""}pollis-media/`;

// Args for the Rust `get_media_path` command. Snake/camel mix mirrors the
// desktop `get_media_url` call exactly (`r2Key`, `contentHash`,
// `contentType`) so the dispatch arm can be shared 1:1 when it's written.
interface GetMediaPathArgs {
  r2Key: string;
  contentHash: string;
  contentType: string;
  // Absolute sandbox dir Rust should decrypt into. Passing it from JS
  // keeps the cache location owned by the platform layer (Expo decides
  // the sandbox path), not hardcoded in Rust.
  destDir: string;
}

// One in-flight resolve per content hash. A second caller for the same
// hash awaits the first instead of kicking off a redundant decrypt.
const inFlight = new Map<string, Promise<string>>();

// How many live consumers currently hold each resolved URI. The file is
// unlinked when this hits zero. Keyed by content hash (content-addressed,
// so identical bytes across messages share one file + one refcount).
const refCounts = new Map<string, number>();

// content hash → resolved `file://` URI, for resolved entries we still
// hold a reference to. Lets `releaseMediaUri` find the path to unlink.
const resolvedUris = new Map<string, string>();

let dirEnsured: Promise<void> | null = null;

// Bumped by every `clearMediaCache()`. A resolve or release that began
// before the clear belongs to the session that ended: a resolve still in
// flight must not register (or hand out) a file written after the wipe, and
// a late release from a view unmounting after sign-out must not decrement a
// count the next session now owns.
let generation = 0;

// The current cache generation. Capture it alongside a resolve and hand it
// back to `releaseMediaUri` so a release that outlives a clear is a no-op.
export function mediaCacheGeneration(): number {
  return generation;
}

// Create the media cache dir once. Idempotent across the app lifetime.
function ensureDir(): Promise<void> {
  if (dirEnsured) {
    return dirEnsured;
  }
  const pending = FileSystem.makeDirectoryAsync(MEDIA_DIR, {
    intermediates: true,
  }).catch((err: unknown) => {
    // Reset so a transient failure (e.g. storage pressure) can retry on
    // the next resolve instead of being cached forever.
    dirEnsured = null;
    throw err;
  });
  dirEnsured = pending;
  return pending;
}

// Resolve a media attachment to a local `file://` URI pointing at the
// decrypted bytes, fetching + decrypting via Rust on first use and
// reusing the cached file thereafter. Increments the reference count —
// every successful resolve must be paired with a `releaseMediaUri()`
// (the `useMediaUri` hook does this on unmount).
export async function resolveMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type"
  >,
): Promise<string> {
  const { object_key, content_hash, content_type } = attachment;

  // Optimistic sends with no object key yet can't be fetched — callers
  // should use `localPreviewUri` for those. Guard so a half-built
  // attachment doesn't reach Rust.
  if (!object_key || !content_hash) {
    throw new Error("resolveMediaUri: attachment has no object_key/content_hash");
  }

  const existing = inFlight.get(content_hash);
  if (existing) {
    // Count this caller too — it shares the same resolved file.
    refCounts.set(content_hash, (refCounts.get(content_hash) ?? 0) + 1);
    return existing;
  }

  const startedIn = generation;
  const promise = (async () => {
    await ensureDir();
    const uri = await invoke<string>("get_media_path", {
      r2Key: object_key,
      contentHash: content_hash,
      contentType: content_type,
      destDir: MEDIA_DIR,
    } satisfies GetMediaPathArgs);
    if (!uri) {
      throw new Error(`get_media_path returned empty path for ${content_hash}`);
    }
    if (startedIn !== generation) {
      // The cache was cleared (sign-out) while Rust was decrypting, so this
      // file landed after the wipe. Unlink it instead of handing plaintext
      // from the ended session to anyone.
      await FileSystem.deleteAsync(uri, { idempotent: true });
      throw new Error(`media cache cleared while resolving ${content_hash}`);
    }
    resolvedUris.set(content_hash, uri);
    return uri;
  })();

  inFlight.set(content_hash, promise);
  refCounts.set(content_hash, (refCounts.get(content_hash) ?? 0) + 1);

  promise.catch(() => {
    // Drop the rejected promise so the next consumer retries instead of
    // re-awaiting a permanent failure. The reference count is reconciled
    // by the consumer's `releaseMediaUri` call in its cleanup. Only drop our
    // own entry — after a clear the slot may hold a newer resolve.
    if (inFlight.get(content_hash) === promise) {
      inFlight.delete(content_hash);
    }
  });

  return promise;
}

// Release one reference to a resolved attachment. When the last consumer
// releases, the decrypted file is unlinked so plaintext doesn't outlive
// the render (issue #346's lifecycle recommendation). Safe to call even
// if the resolve failed. Pass the `mediaCacheGeneration()` captured at
// resolve time: a release from before the last clear has nothing left to
// release, since the clear already unlinked its file.
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
  // `idempotent` so a double-release or an already-evicted file doesn't
  // throw.
  await FileSystem.deleteAsync(uri, { idempotent: true });
}

// ── Named copies (#1248) ──────────────────────────────────────────────
//
// The decrypted cache file is named by content hash alone, with no
// extension. expo-image sniffs the bytes, but AVPlayer (expo-video,
// expo-audio on iOS), the iOS photo library and the share sheet all type a
// file by its extension, and the share sheet shows the name. So the viewer
// takes a second, NAMED copy of the same decrypted bytes — the sender's
// filename, with an extension — under MEDIA_DIR/named/<hash>/. It is never
// fetched from anywhere: it is a local copy of the file `resolveMediaUri`
// produced, ref-counted the same way, and unlinked with its last release
// (or by `clearMediaCache`, which removes MEDIA_DIR wholesale).

const NAMED_DIR = `${MEDIA_DIR}named/`;
const namedInFlight = new Map<string, Promise<string>>();
const namedRefCounts = new Map<string, number>();
// How many base references the named copy holds for each hash: one per
// resolve attempt, so a failed attempt that is retried releases them all.
const namedBaseRefs = new Map<string, number>();

/**
 * Resolve an attachment to a decrypted copy that carries its filename and
 * extension. Takes a reference; pair every call with `releaseNamedMediaUri`.
 * An optimistic send (no object key yet) resolves to its own local preview,
 * which the picker already named.
 */
export async function resolveNamedMediaUri(
  attachment: Pick<
    MessageAttachment,
    "object_key" | "content_hash" | "content_type" | "filename" | "localPreviewUri"
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
    // Let the next caller retry; references are reconciled on release. Only
    // drop our own entry — after a clear the slot may hold a newer copy.
    if (namedInFlight.get(content_hash) === promise) {
      namedInFlight.delete(content_hash);
    }
  });
  return promise;
}

/**
 * Release one reference taken by `resolveNamedMediaUri`. Pass the
 * `mediaCacheGeneration()` captured at resolve time, as for
 * `releaseMediaUri`: a release from before the last clear is a no-op.
 */
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

// Drop every cached media file and reset bookkeeping. For sign-out /
// account-switch teardown, where the sandbox should not retain decrypted
// plaintext from the previous session. Called from `endSession()`
// (lib/session) — every path that ends a session goes through it.
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
