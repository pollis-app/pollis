/* Video posters (#1837): `pv` reaches the attachment, a bad one is dropped, an old message has none, and the tile's source order. #1894: a posterless video is decoded once and saved. */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseContent, posterOf, posterUri } from "../lib/messageContent.ts";
import { editedVaultContent } from "../lib/vault.ts";
import { LEGACY_POSTER_MAX_BYTES, videoPosterPlan } from "../lib/media/videoPoster.ts";
import {
  localVideoPoster,
  type LocalVideoPosterDeps,
  type SavedVideoPoster,
} from "../lib/media/localVideoPoster.ts";

const JPEG = "/9j/4AAQSkZJRgABAQ==";

const entry = (key: string, extra: Record<string, unknown> = {}) => ({
  key,
  name: "clip.mp4",
  ct: "video/mp4",
  size: 3,
  hash: `h-${key}`,
  kv: 1,
  sha: "s",
  bh: "LEHV6nWB2yk8pyo0adR*.7kCMdnj",
  w: 1280,
  h: 720,
  ...extra,
});

test("pv becomes the attachment's poster", () => {
  const [att] = parseContent(JSON.stringify({ _att: [entry("media/a.enc", { pv: JPEG })] }), {
    conversationId: "c1",
  }).attachments;
  assert.equal(att.poster, JPEG);
  assert.equal(posterUri(att.poster!), `data:image/jpeg;base64,${JPEG}`);
});

test("a message from an older sender (no pv) parses exactly as before", () => {
  const [att] = parseContent(JSON.stringify({ _att: [entry("media/a.enc")] }), {
    conversationId: "c1",
  }).attachments;
  assert.equal(att.poster, undefined);
  assert.equal(att.blurhash, "LEHV6nWB2yk8pyo0adR*.7kCMdnj");
  assert.equal(att.width, 1280);
});

test("anything but plain, bounded base64 is ignored", () => {
  assert.equal(posterOf(JPEG), JPEG);
  assert.equal(posterOf(""), undefined);
  assert.equal(posterOf(42), undefined);
  assert.equal(posterOf({ src: JPEG }), undefined);
  assert.equal(posterOf('x" onerror="alert(1)'), undefined);
  assert.equal(posterOf("javascript:alert(1)"), undefined);
  assert.equal(posterOf("A".repeat(64 * 1024 + 4)), undefined);
});

test("a Vault text edit keeps the poster", () => {
  const original = JSON.stringify({ _att: [entry("media/a.enc", { kv: 0, pv: JPEG })], _txt: "old" });
  const edited = JSON.parse(editedVaultContent(original, "new")!);
  assert.equal(edited._att[0].pv, JPEG);
  assert.equal(edited._txt, "new");
});

test("a tile's frame: pv first, then a pending send's file, then a small or already-local legacy file", () => {
  const base = { object_key: "media/a.enc", localPreviewUri: undefined, file_size: 1024 };
  assert.equal(videoPosterPlan({ ...base, poster: JPEG }, false), "pv");
  assert.equal(videoPosterPlan({ ...base, object_key: "", localPreviewUri: "file:///v.mp4" }, false), "pending");
  assert.equal(videoPosterPlan({ ...base, object_key: "" }, false), "none");
  assert.equal(videoPosterPlan(base, false), "legacy");
  const big = { ...base, file_size: LEGACY_POSTER_MAX_BYTES + 1 };
  assert.equal(videoPosterPlan(big, false), "none", "no download just for a frame");
  assert.equal(videoPosterPlan(big, true), "legacy", "already on disk costs nothing");
});

// #1894: a video with no `pv` is decoded once and its poster saved on the device.
function posterStore(frame: string | null = "RlJBTUU=") {
  const table = new Map<string, SavedVideoPoster>();
  const calls = { get: 0, decode: 0, save: 0 };
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

test("a second render reads the saved poster and decodes nothing", async () => {
  const { table, calls, deps } = posterStore();
  const first = await localVideoPoster({ content_hash: "h1" }, deps);
  assert.deepEqual(first, { jpeg: "bounded:RlJBTUU=", blurhash: "LEHV6nWB2yk8" });
  assert.equal(table.size, 1);
  const second = await localVideoPoster({ content_hash: "h1" }, deps);
  assert.deepEqual(second, first);
  assert.equal(calls.decode, 1, "decoded once");
  assert.equal(calls.save, 1);
});

test("a video with pv never reads, decodes or writes a row", async () => {
  const { table, calls, deps } = posterStore();
  assert.equal(await localVideoPoster({ content_hash: "h1", poster: JPEG }, deps), null);
  assert.deepEqual(calls, { get: 0, decode: 0, save: 0 });
  assert.equal(table.size, 0);
});

test("no frame saves nothing; a failed save still draws the frame", async () => {
  const empty = posterStore(null);
  assert.equal(await localVideoPoster({ content_hash: "h1" }, empty.deps), null);
  assert.equal(empty.table.size, 0);

  const refused = posterStore();
  refused.deps.save = async () => {
    throw new Error("Not signed in");
  };
  assert.deepEqual(await localVideoPoster({ content_hash: "h1" }, refused.deps), {
    jpeg: "RlJBTUU=",
    blurhash: null,
  });
});

test("the tile reads the saved poster through the core, and the rules match desktop's", () => {
  const hook = readFileSync(new URL("../hooks/useLocalVideoPoster.ts", import.meta.url), "utf8");
  assert.match(hook, /"get_local_video_poster"/);
  assert.match(hook, /"save_local_video_poster"/);
  const code = (s: string) => s.replace(/^\/\/.*\n/gm, "");
  const mobile = readFileSync(new URL("../lib/media/localVideoPoster.ts", import.meta.url), "utf8");
  const desktop = readFileSync(new URL("../../frontend/src/utils/localVideoPoster.ts", import.meta.url), "utf8");
  assert.equal(code(mobile), code(desktop));
});
