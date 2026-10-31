// Rapid sends queue and leave in tap order (#1636); mobile carries the same module.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createSendLine } from "../src/utils/sendLine.ts";

const tick = () => new Promise<void>((r) => setTimeout(r, 0));

test("sends in one conversation run one at a time, in the order their turns were taken", async () => {
  const line = createSendLine();
  const log: string[] = [];
  const turns = ["a", "b", "c"].map((name) => ({ name, turn: line.take("c1") }));
  // Started in reverse, as when a later send finishes preparing its attachments first.
  const runs = [...turns].reverse().map(async ({ name, turn }) => {
    await turn.ready;
    log.push(`start ${name}`);
    await tick();
    log.push(`end ${name}`);
    turn.release();
  });
  await Promise.all(runs);
  assert.deepEqual(log, ["start a", "end a", "start b", "end b", "start c", "end c"]);
});

test("a failed or abandoned send never stalls the ones behind it", async () => {
  const line = createSendLine();
  const a = line.take("c1");
  const b = line.take("c1");
  a.release();
  a.release();
  await b.ready;
  b.release();
  await tick();
  assert.deepEqual(line.busy(), []);
});

test("desktop and mobile share one send-line implementation", () => {
  const desktop = readFileSync(new URL("../src/utils/sendLine.ts", import.meta.url), "utf8");
  const mobile = readFileSync(new URL("../../mobile/lib/sendLine.ts", import.meta.url), "utf8");
  assert.equal(desktop, mobile);
});

test("both desktop composers take the turn before any await and pass it to the send", () => {
  for (const file of ["Layout/MainContent.tsx", "Layout/RightPanel/ThreadPanel.tsx"]) {
    const src = readFileSync(new URL(`../src/components/${file}`, import.meta.url), "utf8");
    assert.match(src, /const turn = takeSendTurn\(/, file);
    assert.match(src, /turn,\n/, file);
    assert.match(src, /turn\.release\(\);/, file);
  }
  const hook = readFileSync(new URL("../src/hooks/queries/useMessages.ts", import.meta.url), "utf8");
  assert.match(hook, /await turn\.ready;/);
  assert.match(hook, /variables\.turn\?\.release\(\);/);
});
