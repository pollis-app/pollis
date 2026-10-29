/* Pinned messages on mobile (#1315): the snapshot parse, the bounded jump, a one-shot jump store. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { pinPreview, pinnedIds } from "../lib/pins.ts";
import { parseContent } from "../lib/messageContent.ts";
import { JUMP_MAX_PAGES, nextJumpStep } from "../lib/messageJump.ts";
import { messageJumpStore } from "../stores/messageJumpStore.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

test("a plain-text pin previews as its text", () => {
  assert.deepEqual(pinPreview("pin me"), { text: "pin me", filenames: [] });
});

test("an attachment pin previews its caption and file names, never the envelope", () => {
  const content = JSON.stringify({
    _att: [
      { key: "media/a", hash: "h1", name: "a.png", ct: "image/png", size: 1 },
      { key: "media/b", hash: "h2", name: "b.pdf", ct: "application/pdf", size: 2 },
    ],
    _txt: "two files",
  });
  assert.deepEqual(pinPreview(content), { text: "two files", filenames: ["a.png", "b.pdf"] });
  const bare = JSON.stringify({
    _att: [{ key: "media/a", hash: "h", name: "a.png", ct: "image/png", size: 1 }],
  });
  assert.deepEqual(pinPreview(bare), { text: "", filenames: ["a.png"] });
});

test("a pin and a timeline row parse content identically", () => {
  for (const raw of ["hi", '{"_txt":"legacy"}', "{not json", '{"x":1}']) {
    assert.equal(pinPreview(raw).text, parseContent(raw, null).text, raw);
  }
});

test("pinned ids are keyed on the message, not the pin row", () => {
  const ids = pinnedIds([{ message_id: "m1" }, { message_id: "m2" }]);
  assert.ok(ids.has("m1") && ids.has("m2"));
  assert.equal(ids.size, 2);
});

const base = {
  index: -1,
  loaded: false,
  isLoading: false,
  isFetchingNextPage: false,
  hasNextPage: true,
  pageCount: 1,
};

test("a rendered row is scrolled to", () => {
  assert.deepEqual(nextJumpStep({ ...base, index: 7, loaded: true }), { kind: "scroll", index: 7 });
});

test("an unloaded row pages back, one page at a time", () => {
  assert.deepEqual(nextJumpStep(base), { kind: "loadOlder" });
  assert.deepEqual(nextJumpStep({ ...base, isFetchingNextPage: true }), { kind: "wait" });
  assert.deepEqual(nextJumpStep({ ...base, isLoading: true, pageCount: 0 }), { kind: "wait" });
});

test("the walk ends: no older history, or the page cap", () => {
  assert.deepEqual(nextJumpStep({ ...base, hasNextPage: false }), { kind: "giveUp" });
  assert.deepEqual(nextJumpStep({ ...base, pageCount: JUMP_MAX_PAGES }), { kind: "giveUp" });
});

test("a loaded message that is not a timeline row (a thread reply) is not chased", () => {
  assert.deepEqual(nextJumpStep({ ...base, loaded: true }), { kind: "giveUp" });
});

test("a jump fires once, and only for the message it was queued for", () => {
  messageJumpStore.request("c1", "m1");
  assert.equal(messageJumpStore.claim("m2"), false);
  assert.equal(messageJumpStore.claim("m1"), true);
  assert.equal(messageJumpStore.claim("m1"), false);
  assert.equal(messageJumpStore.conversationId, null);
});

test("sign-out drops a pending jump", () => {
  assert.match(src("stores/appStore.ts"), /logout\(\) \{[\s\S]*messageJumpStore\.reset\(\);/);
});

test("the message sheet pins, the header opens the list, and every jump door queues", () => {
  const chat = src("app/chat/[id].tsx");
  assert.match(chat, /testID="btn-pins"/);
  assert.match(chat, /pathname: "\/conversation\/pins"/);
  assert.match(chat, /onTogglePin=\{/);
  assert.match(src("components/chat/MessageActionsSheet.tsx"), /testID="btn-pin"/);
  for (const door of ["app/conversation/pins.tsx", "app/self/saved.tsx", "app/m/[...permalink].tsx"]) {
    assert.match(src(door), /messageJumpStore\.request\(/, door);
  }
  // Pins have no realtime event: the list reads fresh.
  assert.match(src("app/conversation/pins.tsx"), /usePinnedMessages\(conversationId, \{\s*fresh: true,/);
  assert.match(src("hooks/queries/usePins.ts"), /refetchOnMount: fresh \? "always" : true/);
  assert.match(src("components/pane/paneContext.ts"), /"\/conversation\/pins"/);
  assert.match(src("components/MasterDetail.tsx"), /"\/conversation\/pins": PinnedMessages/);
});
