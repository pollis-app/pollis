// Rapid sends queue instead of disabling the composer, and leave in tap order (#1636).

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createSendLine } from "../lib/sendLine.ts";

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

test("the first turn of a conversation is ready at once", async () => {
  const line = createSendLine();
  const turn = line.take("c1");
  let ready = false;
  void turn.ready.then(() => {
    ready = true;
  });
  await tick();
  assert.equal(ready, true);
});

test("each turn waits for the one before it, so sends run one at a time in tap order", async () => {
  const line = createSendLine();
  const log: string[] = [];
  const turns = ["a", "b", "c"].map((name) => ({ name, turn: line.take("c1") }));
  const runs = turns.map(async ({ name, turn }) => {
    await turn.ready;
    log.push(`start ${name}`);
    await tick();
    log.push(`end ${name}`);
    turn.release();
  });
  await Promise.all(runs);
  assert.deepEqual(log, ["start a", "end a", "start b", "end b", "start c", "end c"]);
});

test("a later send finishing its own preparation first still waits its turn", async () => {
  const line = createSendLine();
  const first = line.take("c1");
  const second = line.take("c1");
  let secondReady = false;
  void second.ready.then(() => {
    secondReady = true;
  });
  await tick();
  assert.equal(secondReady, false);
  first.release();
  await tick();
  assert.equal(secondReady, true);
});

test("a failed send releases its turn like a delivered one: nobody behind it is stuck", async () => {
  const line = createSendLine();
  const failing = line.take("c1");
  const next = line.take("c1");
  const attempt = (async () => {
    await failing.ready;
    try {
      throw new Error("network");
    } finally {
      failing.release();
    }
  })();
  await assert.rejects(attempt);
  await next.ready;
  next.release();
});

test("release is idempotent and an early release keeps the order", async () => {
  const line = createSendLine();
  const a = line.take("c1");
  const b = line.take("c1");
  const c = line.take("c1");
  // b gave up before its turn came (its onMutate threw): c must still wait for a.
  b.release();
  b.release();
  let cReady = false;
  void c.ready.then(() => {
    cReady = true;
  });
  await tick();
  assert.equal(cReady, false);
  a.release();
  await tick();
  assert.equal(cReady, true);
});

test("conversations do not wait on each other", async () => {
  const line = createSendLine();
  line.take("c1");
  const other = line.take("c2");
  let ready = false;
  void other.ready.then(() => {
    ready = true;
  });
  await tick();
  assert.equal(ready, true);
});

test("a drained line forgets the conversation", async () => {
  const line = createSendLine();
  const a = line.take("c1");
  const b = line.take("c1");
  assert.deepEqual(line.busy(), ["c1"]);
  a.release();
  b.release();
  await tick();
  assert.deepEqual(line.busy(), []);
});

test("the chat and thread composers never hold the send button on an in-flight send", () => {
  for (const file of ["app/chat/[id].tsx", "app/chat/thread.tsx"]) {
    const src = readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
    assert.doesNotMatch(src, /sendMessage\.isPending/, `${file} gates sending on an in-flight send`);
  }
});

test("useSendMessage takes the turn at the call and releases it on every outcome", () => {
  const src = readFileSync(new URL("../hooks/queries/useMessages.ts", import.meta.url), "utf8");
  assert.match(src, /sendLine\.take\(conversationId\)/);
  assert.match(src, /await vars\.turn\.ready;/);
  assert.match(src, /onSettled: \(_data, _e, vars\) => \{\s*vars\.turn\?\.release\(\);/);
  // Two stubs made in one millisecond must not share an id, or settling one replaces both.
  assert.match(src, /`pending-\$\{Date\.now\(\)\}-\$\{stubSeq\}`/);
});
