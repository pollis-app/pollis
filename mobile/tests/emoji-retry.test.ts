import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  EMOJI_RETRY_DELAYS_MS,
  emojiRetryDelay,
  retryOnForeground,
} from "../lib/emojiRetry.ts";
import { reportEmojiShown, shortcodesShowing } from "../lib/emojiRenderState.ts";

test("the e2e probe state counts every mounted emoji and forgets unmounted ones", () => {
  const a = reportEmojiShown("probe_a", "image");
  const b = reportEmojiShown("probe_a", "image");
  const c = reportEmojiShown("probe_b", "fallback");
  reportEmojiShown("probe_c", null)();
  assert.deepEqual(shortcodesShowing("image"), ["probe_a"]);
  assert.deepEqual(shortcodesShowing("fallback"), ["probe_b"]);
  a();
  a();
  assert.deepEqual(shortcodesShowing("image"), ["probe_a"]);
  b();
  c();
  assert.deepEqual(shortcodesShowing("image"), []);
  assert.deepEqual(shortcodesShowing("fallback"), []);
});

test("a failed emoji retries on a short, finite schedule (#1562)", () => {
  assert.equal(emojiRetryDelay(0), null);
  assert.equal(emojiRetryDelay(1), EMOJI_RETRY_DELAYS_MS[0]);
  assert.equal(emojiRetryDelay(EMOJI_RETRY_DELAYS_MS.length), EMOJI_RETRY_DELAYS_MS.at(-1));
  assert.equal(emojiRetryDelay(EMOJI_RETRY_DELAYS_MS.length + 1), null);
  for (let i = 1; i < EMOJI_RETRY_DELAYS_MS.length; i++) {
    assert.ok(EMOJI_RETRY_DELAYS_MS[i] > EMOJI_RETRY_DELAYS_MS[i - 1]);
  }
});

test("returning to the foreground retries only a shown fallback with no timer pending", () => {
  assert.equal(retryOnForeground(true, false), true);
  assert.equal(retryOnForeground(true, true), false);
  assert.equal(retryOnForeground(false, false), false);
});

// #1562: a source-less RN Image fires onError on iOS, latching the fallback.
test("CustomEmojiImage never mounts an <Image> without a source", () => {
  const src = readFileSync(
    fileURLToPath(new URL("../components/emoji/CustomEmojiImage.tsx", import.meta.url)),
    "utf8",
  );
  assert.ok(!/source=\{uri \? \{ uri \} : undefined\}/.test(src));
  assert.match(src, /<Image\s+source=\{\{ uri \}\}/);
});
