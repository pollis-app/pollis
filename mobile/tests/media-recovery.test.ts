/* A failed media tile is never sticky (#1826): it recovers when anyone resolves the same file, and a remount re-asks. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createMediaStore } from "../lib/media/store.ts";
import { startMediaTile, type MediaTileState } from "../lib/media/tile.ts";

const settle = () => new Promise((resolve) => setImmediate(resolve));

/** A store whose decrypt fails while `offline` is set, like `get_media_path` on a dropped network. */
function harness() {
  const unlinked: string[] = [];
  const decrypts: string[] = [];
  let offline = false;
  const store = createMediaStore({
    unlink: async (uri) => {
      unlinked.push(uri);
    },
    retainMax: 4,
  });
  const decrypt = (hash: string) => () => {
    decrypts.push(hash);
    if (offline) {
      return Promise.reject(new Error("network unreachable"));
    }
    return Promise.resolve(`file:///media/${hash}`);
  };
  /** Mount one tile, as `useMediaUri` does; `states` is everything it rendered. */
  const mount = (hash: string) => {
    const states: MediaTileState[] = [];
    const unmount = startMediaTile(
      hash,
      {
        peek: (h) => store.peek(h),
        resolve: () => store.acquire(hash, decrypt(hash)),
        release: (generation) => {
          void store.release(hash, generation);
        },
        generation: () => store.generation(),
        onResolved: (h, listener) => store.subscribe(h, listener),
      },
      (state) => states.push(state),
    );
    return { states, unmount, last: () => states[states.length - 1] };
  };
  return {
    store,
    unlinked,
    decrypts,
    decrypt,
    mount,
    setOffline: (value: boolean) => {
      offline = value;
    },
  };
}

test("a failed tile renders the file as soon as another consumer resolves it", async () => {
  const { store, mount, decrypt, setOffline } = harness();
  setOffline(true);
  const row = mount("a");
  await settle();
  assert.equal(row.last().error?.message, "network unreachable");
  assert.equal(row.last().uri, null);
  assert.equal(store.listenerCount(), 1);

  // The network is back and the viewer opens the same file.
  setOffline(false);
  const viewer = mount("a");
  await settle();
  assert.equal(viewer.last().uri, "file:///media/a");

  // The row picked it up from the event: rendered, no error, no longer listening.
  assert.deepEqual(row.last(), { uri: "file:///media/a", loading: false, error: null });
  assert.equal(store.listenerCount(), 0);

  // Both consumers hold it; it is kept only after both leave.
  viewer.unmount();
  await settle();
  assert.deepEqual(store.retained(), []);
  row.unmount();
  await settle();
  assert.deepEqual(store.retained(), ["a"]);
});

test("the recovery reuses the other consumer's decrypt rather than fetching again", async () => {
  const { store, mount, decrypts, decrypt, setOffline } = harness();
  setOffline(true);
  const row = mount("a");
  await settle();
  setOffline(false);
  // Any resolve counts, not just a tile's: here a save from the viewer's toolbar.
  await store.acquire("a", decrypt("a"));
  await settle();
  assert.equal(row.last().uri, "file:///media/a");
  // One failed attempt, one success; the row's retry was answered from the cache.
  assert.deepEqual(decrypts, ["a", "a"]);
  row.unmount();
});

test("a tile for another file is not woken", async () => {
  const { store, mount, setOffline } = harness();
  setOffline(true);
  const row = mount("a");
  await settle();
  setOffline(false);
  const other = mount("b");
  await settle();
  assert.equal(other.last().uri, "file:///media/b");
  assert.equal(row.last().uri, null);
  assert.ok(row.last().error);
  assert.equal(store.listenerCount(), 1);
  row.unmount();
  other.unmount();
  assert.equal(store.listenerCount(), 0);
});

test("a remount after a failure asks the cache again, and the fetch is retried", async () => {
  const { mount, decrypts, setOffline } = harness();
  setOffline(true);
  const first = mount("a");
  await settle();
  assert.ok(first.last().error);
  first.unmount();

  setOffline(false);
  const second = mount("a");
  // Mounts loading, never in the old failed state.
  assert.deepEqual(second.states[0], { uri: null, loading: true, error: null });
  await settle();
  assert.deepEqual(second.last(), { uri: "file:///media/a", loading: false, error: null });
  assert.deepEqual(decrypts, ["a", "a"]);
  second.unmount();
});

test("a remount while the file is held elsewhere draws it on the first state, with no fetch", async () => {
  const { mount, decrypts } = harness();
  const viewer = mount("a");
  await settle();
  const row = mount("a");
  assert.deepEqual(row.states[0], { uri: "file:///media/a", loading: false, error: null });
  await settle();
  assert.deepEqual(decrypts, ["a"]);
  row.unmount();
  viewer.unmount();
});

test("unmounting a failed tile drops its listener and its reference", async () => {
  const { store, mount, decrypt, setOffline, unlinked } = harness();
  setOffline(true);
  const row = mount("a");
  await settle();
  assert.equal(store.listenerCount(), 1);
  row.unmount();
  assert.equal(store.listenerCount(), 0);

  // A later resolve elsewhere wakes nobody and the file is kept normally once released.
  setOffline(false);
  const uri = await store.acquire("a", decrypt("a"));
  await store.release("a");
  assert.deepEqual(store.retained(), ["a"]);
  assert.equal(store.peek("a"), uri);
  assert.deepEqual(unlinked, []);
});

test("a failure from before a sign-out clear neither releases nor wakes into the next session", async () => {
  const { store, mount, decrypt, setOffline } = harness();
  setOffline(true);
  const row = mount("a");
  await settle();
  store.clear();
  setOffline(false);
  // Next session resolves the same hash: the old tile retries under the new generation.
  await store.acquire("a", decrypt("a"));
  await settle();
  assert.equal(row.last().uri, "file:///media/a");
  row.unmount();
  await settle();
  // The next session's own consumer still holds it: nothing kept-for-revisit yet.
  assert.deepEqual(store.retained(), []);
});

test("useMediaUri and useNamedMediaUri both run through the tile, and MediaImage keeps memory-only caching", () => {
  for (const hook of ["useMediaUri", "useNamedMediaUri"]) {
    const src = readFileSync(new URL(`../hooks/${hook}.ts`, import.meta.url), "utf8");
    assert.match(src, /startMediaTile\(/, `${hook} must use startMediaTile`);
    assert.match(src, /subscribeMediaResolved/, `${hook} must listen for resolves`);
    assert.doesNotMatch(src, /setInterval|setTimeout/, `${hook} must not poll`);
  }
  const tile = readFileSync(new URL("../lib/media/tile.ts", import.meta.url), "utf8");
  assert.doesNotMatch(tile, /setInterval|setTimeout/);
  const media = readFileSync(new URL("../components/Media.tsx", import.meta.url), "utf8");
  assert.match(media, /cachePolicy="memory"/);
});
