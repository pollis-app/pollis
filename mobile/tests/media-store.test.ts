/* Decrypted media (lib/media/store.ts): ref-counted, a few just-released files kept for a remount. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createMediaStore } from "../lib/media/store.ts";

function harness(retainMax = 2) {
  const unlinked: string[] = [];
  const decrypts: string[] = [];
  const store = createMediaStore({
    unlink: async (uri) => {
      unlinked.push(uri);
    },
    retainMax,
  });
  const decrypt = (hash: string) => () => {
    decrypts.push(hash);
    return Promise.resolve(`file:///media/${hash}`);
  };
  return { store, unlinked, decrypts, decrypt };
}

test("a remount after the last release finds the file synchronously, with no second decrypt", async () => {
  const { store, unlinked, decrypts, decrypt } = harness();
  assert.equal(store.peek("a"), null);
  const uri = await store.acquire("a", decrypt("a"));
  assert.equal(store.peek("a"), uri);
  // The Chat tab unmounts its tile...
  await store.release("a");
  assert.deepEqual(unlinked, []);
  // ...and the Media tab's tile mounts: the path is known on its first render.
  assert.equal(store.peek("a"), uri);
  assert.equal(await store.acquire("a", decrypt("a")), uri);
  assert.deepEqual(decrypts, ["a"]);
  assert.deepEqual(store.retained(), []);
});

test("consumers share one file; it is kept only once the last one leaves", async () => {
  const { store, decrypts, decrypt } = harness();
  await Promise.all([store.acquire("a", decrypt("a")), store.acquire("a", decrypt("a"))]);
  assert.deepEqual(decrypts, ["a"]);
  await store.release("a");
  assert.deepEqual(store.retained(), []);
  await store.release("a");
  assert.deepEqual(store.retained(), ["a"]);
});

test("the kept set is bounded: the least recently released file is unlinked first", async () => {
  const { store, unlinked, decrypt } = harness(2);
  for (const h of ["a", "b", "c"]) {
    await store.acquire(h, decrypt(h));
  }
  await store.release("a");
  await store.release("b");
  // A revisit of "a" refreshes it, so "b" is now the oldest.
  await store.acquire("a", decrypt("a"));
  await store.release("a");
  await store.release("c");
  assert.deepEqual(unlinked, ["file:///media/b"]);
  assert.deepEqual(store.retained(), ["a", "c"]);
  assert.equal(store.peek("b"), null);
});

test("a file in use is never evicted, however many others are released", async () => {
  const { store, unlinked, decrypt } = harness(1);
  await store.acquire("held", decrypt("held"));
  for (const h of ["x", "y", "z"]) {
    await store.acquire(h, decrypt(h));
    await store.release(h);
  }
  assert.ok(!unlinked.includes("file:///media/held"));
  assert.equal(store.peek("held"), "file:///media/held");
});

test("trim (lock, background) unlinks every kept file but none in use", async () => {
  const { store, unlinked, decrypt } = harness(8);
  await store.acquire("on-screen", decrypt("on-screen"));
  await store.acquire("gone", decrypt("gone"));
  await store.release("gone");
  await store.trim();
  assert.deepEqual(unlinked, ["file:///media/gone"]);
  assert.equal(store.peek("gone"), null);
  assert.equal(store.peek("on-screen"), "file:///media/on-screen");
});

test("a consumer that left mid-decrypt leaves the file kept, not leaked", async () => {
  const { store, unlinked } = harness(1);
  let finish: (uri: string) => void = () => {};
  const pending = store.acquire("a", () => new Promise<string>((r) => (finish = r)));
  await store.release("a");
  finish("file:///media/a");
  await pending;
  assert.deepEqual(store.retained(), ["a"]);
  await store.trim();
  assert.deepEqual(unlinked, ["file:///media/a"]);
});

test("sign-out mid-decrypt: the late file is unlinked and handed to no one", async () => {
  const { store, unlinked } = harness();
  let finish: (uri: string) => void = () => {};
  const before = store.generation();
  const pending = store.acquire("a", () => new Promise<string>((r) => (finish = r)));
  store.clear();
  finish("file:///media/a");
  await assert.rejects(pending);
  assert.deepEqual(unlinked, ["file:///media/a"]);
  assert.equal(store.peek("a"), null);
  // A release from the ended session is a no-op.
  await store.release("a", before);
  assert.deepEqual(store.retained(), []);
});

test("a failed decrypt is retried by the next consumer", async () => {
  const { store, decrypt } = harness();
  await assert.rejects(store.acquire("a", () => Promise.reject(new Error("offline"))));
  await store.release("a");
  assert.equal(await store.acquire("a", decrypt("a")), "file:///media/a");
});

test("the image renders a kept file on its first frame: no placeholder, no fade, memory-only cache", () => {
  const media = readFileSync(new URL("../components/Media.tsx", import.meta.url), "utf8");
  assert.match(media, /cachePolicy="memory"/);
  assert.match(media, /transition=\{instant \? 0 : 150\}/);
  assert.match(media, /attachment\.blurhash && !instant/);
  const hook = readFileSync(new URL("../hooks/useMediaUri.ts", import.meta.url), "utf8");
  assert.match(hook, /useState<MediaUriState>\(\(\) => \{\s*const cached = [^;]*peekMediaUri/);
  // Lock and leaving the app drop kept files and decoded copies; sign-out drops everything.
  const cache = readFileSync(new URL("../lib/media/cache.ts", import.meta.url), "utf8");
  assert.match(cache, /state === "background"\) \{\s*void trimMediaCache\(\);/);
  assert.match(cache, /\(\) => appStore\.isLocked,\s*\(locked\) => \{\s*if \(locked\) \{\s*void trimMediaCache\(\);/);
  assert.match(cache, /export async function clearMediaCache[\s\S]*?store\.clear\(\);\s*Image\.clearMemoryCache\(\)/);
  const layout = readFileSync(new URL("../app/_layout.tsx", import.meta.url), "utf8");
  assert.match(layout, /startMediaRetention\(\);/);
});
