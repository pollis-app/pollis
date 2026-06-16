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
    resolvedUris.set(content_hash, uri);
    return uri;
  })();

  inFlight.set(content_hash, promise);
  refCounts.set(content_hash, (refCounts.get(content_hash) ?? 0) + 1);

  promise.catch(() => {
    // Drop the rejected promise so the next consumer retries instead of
    // re-awaiting a permanent failure. The reference count is reconciled
    // by the consumer's `releaseMediaUri` call in its cleanup.
    inFlight.delete(content_hash);
  });

  return promise;
}

// Release one reference to a resolved attachment. When the last consumer
// releases, the decrypted file is unlinked so plaintext doesn't outlive
// the render (issue #346's lifecycle recommendation). Safe to call even
// if the resolve failed.
export async function releaseMediaUri(contentHash: string): Promise<void> {
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

// Drop every cached media file and reset bookkeeping. For sign-out /
// account-switch teardown, where the sandbox should not retain decrypted
// plaintext from the previous session.
export async function clearMediaCache(): Promise<void> {
  inFlight.clear();
  refCounts.clear();
  resolvedUris.clear();
  dirEnsured = null;
  await FileSystem.deleteAsync(MEDIA_DIR, { idempotent: true });
}
