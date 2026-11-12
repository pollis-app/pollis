/* A failed media tile is never sticky (#1826): it recovers when anyone resolves or draws the same file, and a remount re-asks. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { createMediaUrlCache } from "../src/services/mediaUrlCache.ts";

const settle = () => new Promise((resolve) => setImmediate(resolve));

/** `get_media_url` that fails while `offline` is set. */
function harness() {
  const cache = createMediaUrlCache();
  const fetches: string[] = [];
  let offline = false;
  const fetch = (hash: string) => () => {
    fetches.push(hash);
    if (offline) {
      return Promise.reject(new Error("network unreachable"));
    }
    return Promise.resolve(`http://media.localhost/${hash}`);
  };

  /** One row as AttachmentDisplay drives it: fetch on mount, listen while failed. */
  const mountTile = (hash: string) => {
    const tile = { url: cache.peek(hash), error: null as string | null };
    let unsubscribe: (() => void) | null = null;
    if (!tile.url) {
      cache.get(hash, fetch(hash)).then(
        (url) => {
          tile.url = url;
        },
        (err: Error) => {
          tile.error = err.message;
          unsubscribe = cache.subscribe(hash, (url) => {
            unsubscribe?.();
            unsubscribe = null;
            tile.error = null;
            tile.url = url;
          });
        },
      );
    }
    return {
      tile,
      unmount: () => {
        unsubscribe?.();
        unsubscribe = null;
      },
    };
  };

  return {
    cache,
    fetches,
    fetch,
    mountTile,
    setOffline: (value: boolean) => {
      offline = value;
    },
  };
}

test("a failed row renders the file as soon as the spotlight or another row resolves it", async () => {
  const { cache, mountTile, setOffline } = harness();
  setOffline(true);
  const row = mountTile("a");
  await settle();
  assert.equal(row.tile.error, "network unreachable");
  assert.equal(cache.listenerCount(), 1);

  setOffline(false);
  const other = mountTile("a");
  await settle();
  assert.equal(other.tile.url, "http://media.localhost/a");
  assert.deepEqual(row.tile, { url: "http://media.localhost/a", error: null });
  assert.equal(cache.listenerCount(), 0);
});

test("an image drawn elsewhere from an already-resolved URL wakes a failed row too", async () => {
  const { cache, mountTile, setOffline } = harness();
  setOffline(true);
  const row = mountTile("a");
  await settle();
  cache.announce("a", "http://media.localhost/a");
  assert.equal(row.tile.url, "http://media.localhost/a");
  assert.equal(row.tile.error, null);
});

test("a remount after a failure asks again and the fetch is retried", async () => {
  const { cache, fetches, mountTile, setOffline } = harness();
  setOffline(true);
  const first = mountTile("a");
  await settle();
  assert.ok(first.tile.error);
  first.unmount();
  assert.equal(cache.listenerCount(), 0);

  setOffline(false);
  const second = mountTile("a");
  assert.equal(second.tile.error, null);
  await settle();
  assert.equal(second.tile.url, "http://media.localhost/a");
  assert.deepEqual(fetches, ["a", "a"]);
  // A third mount draws from the cache with no fetch.
  const third = mountTile("a");
  assert.equal(third.tile.url, "http://media.localhost/a");
  assert.deepEqual(fetches, ["a", "a"]);
});

test("a success for one file wakes no tile failing on another", async () => {
  const { cache, mountTile, setOffline } = harness();
  setOffline(true);
  const row = mountTile("a");
  await settle();
  setOffline(false);
  mountTile("b");
  await settle();
  assert.ok(row.tile.error);
  row.unmount();
  assert.equal(cache.listenerCount(), 0);
});

test("the row and the spotlight subscribe only while failed, and never poll", () => {
  const row = readFileSync(new URL("../src/components/Message/AttachmentDisplay.tsx", import.meta.url), "utf8");
  const spotlight = readFileSync(new URL("../src/components/Message/MediaSpotlight.tsx", import.meta.url), "utf8");
  assert.match(row, /if \(!error \|\| !attachment\.object_key\) \{\s*return;\s*\}\s*return subscribeMediaResolved\(/);
  assert.match(spotlight, /if \(!failed \|\| !attachment\.object_key\) \{\s*return;\s*\}\s*return subscribeMediaResolved\(/);
  for (const src of [row, spotlight]) {
    assert.match(src, /announceMediaLoaded\(/);
    assert.doesNotMatch(src, /setInterval/);
  }
  const upload = readFileSync(new URL("../src/services/r2-upload.ts", import.meta.url), "utf8");
  assert.match(upload, /mediaUrls\.get\(attachment\.content_hash/);
});
