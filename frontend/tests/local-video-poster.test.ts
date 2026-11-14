/* #1894: a video with no `pv` is decoded once; later renders read the saved poster, and a video with `pv` never writes a row. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  localVideoPoster,
  type LocalVideoPosterDeps,
  type SavedVideoPoster,
} from "../src/utils/localVideoPoster.ts";

const HASH = "ab12cd34";

/** The local table as a Map, and a decoder that counts its calls. */
function harness(frame: string | null = "RlJBTUU=") {
  const table = new Map<string, SavedVideoPoster>();
  const calls = { decode: 0, save: 0, get: 0 };
  const deps: LocalVideoPosterDeps = {
    get: async (hash) => {
      calls.get += 1;
      return table.get(hash) ?? null;
    },
    decode: async () => {
      calls.decode += 1;
      return frame;
    },
    save: async (hash, f) => {
      calls.save += 1;
      const saved = { jpeg: `bounded:${f}`, blurhash: "LEHV6nWB2yk8", width: 640, height: 360 };
      table.set(hash, saved);
      return saved;
    },
  };
  return { table, calls, deps };
}

test("the first render decodes and saves; the second reads the saved poster and decodes nothing", async () => {
  const { table, calls, deps } = harness();
  const video = { content_hash: HASH };

  const first = await localVideoPoster(video, deps);
  assert.deepEqual(first, { jpeg: "bounded:RlJBTUU=", blurhash: "LEHV6nWB2yk8" });
  assert.equal(calls.decode, 1);
  assert.equal(table.size, 1);

  const second = await localVideoPoster(video, deps);
  assert.deepEqual(second, first);
  assert.equal(calls.decode, 1, "a second render must not decode again");
  assert.equal(calls.save, 1);
});

test("tiles of one video mounting together share one decode", async () => {
  const { calls, deps } = harness();
  const video = { content_hash: "together" };
  const [a, b, c] = await Promise.all([1, 2, 3].map(() => localVideoPoster(video, deps)));
  assert.deepEqual(a, b);
  assert.deepEqual(b, c);
  assert.equal(calls.decode, 1);
  assert.equal(calls.save, 1);
});

test("a video with `pv` never reads, decodes or writes a row", async () => {
  const { table, calls, deps } = harness();
  assert.equal(await localVideoPoster({ content_hash: HASH, poster: "UFY=" }, deps), null);
  assert.deepEqual(calls, { decode: 0, save: 0, get: 0 });
  assert.equal(table.size, 0);
});

test("no frame saves nothing, and the next render tries again", async () => {
  const { table, calls, deps } = harness(null);
  assert.equal(await localVideoPoster({ content_hash: HASH }, deps), null);
  assert.equal(table.size, 0);
  await localVideoPoster({ content_hash: HASH }, deps);
  assert.equal(calls.decode, 2);
});

test("a failed save still draws the decoded frame", async () => {
  const { deps } = harness();
  deps.save = async () => {
    throw new Error("Not signed in");
  };
  assert.deepEqual(await localVideoPoster({ content_hash: HASH }, deps), {
    jpeg: "RlJBTUU=",
    blurhash: null,
  });
});

test("the tile goes through the saved poster, not a bare decode", () => {
  const src = readFileSync(new URL("../src/components/Message/AttachmentDisplay.tsx", import.meta.url), "utf8");
  assert.match(src, /localVideoPoster\(attachment,/);
  assert.match(src, /"get_local_video_poster"/);
  assert.match(src, /"save_local_video_poster"/);
});
