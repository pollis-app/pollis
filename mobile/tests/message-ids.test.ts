/* Message ids named ahead of the tap (#2141): a send's stub carries the core's id from its
 * first frame. Desktop's copy is frontend/src/utils/messageIds.ts, byte for byte. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { createMessageIdPool } from "../lib/messageIds.ts";

const here = dirname(fileURLToPath(import.meta.url));
const settle = () => new Promise((r) => setTimeout(r, 0));

test("mobile/lib/messageIds.ts is desktop's messageIds.ts, byte for byte", () => {
  const mobile = readFileSync(join(here, "../lib/messageIds.ts"), "utf8");
  const desktop = readFileSync(join(here, "../../frontend/src/utils/messageIds.ts"), "utf8");
  assert.equal(mobile, desktop, "edit frontend/src/utils/messageIds.ts and copy it to mobile/lib/messageIds.ts");
});

test("a filled pool hands out each id once, then refills", async () => {
  let n = 0;
  const pool = createMessageIdPool(async () => `id-${(n += 1)}`, 2);
  assert.equal(pool.take(), null, "nothing ready before the first fill lands");
  await settle();
  const a = pool.take();
  const b = pool.take();
  assert.ok(a && b && a !== b, `${a} ${b}`);
  await settle();
  const c = pool.take();
  assert.ok(c && c !== a && c !== b, `${c}`);
  // Never more requests out than the pool holds.
  assert.ok(n <= 5, `${n} mints`);
});

test("a failing core leaves the pool empty and the caller asks directly", async () => {
  const pool = createMessageIdPool(async () => {
    throw new Error("locked");
  }, 2);
  pool.fill();
  await settle();
  assert.equal(pool.take(), null);
});

test("both send paths take a ready id before asking the core", () => {
  const read = (p: string) => readFileSync(join(here, p), "utf8");
  assert.match(read("../hooks/queries/useMessages.ts"), /messageIdPool\.take\(\)/);
  assert.match(read("../../frontend/src/components/Layout/MainContent.tsx"), /messageIdPool\.take\(\)/);
});
