// Row identity across refetches (#2167): an unchanged row keeps its object, so its memoised row skips
// the render; any changed row, a failed send above all, gets a new one, so it always repaints.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { freezeMessage, shareMessagePage, shareMessagesById, type MessagePageData } from "../src/utils/messageSharing.ts";
import { transformChannelMessage, type RawChannelMessage } from "../src/utils/messageTransform.ts";

function raw(i: number, extra: Partial<RawChannelMessage> = {}): RawChannelMessage {
  return {
    id: `01J${String(i).padStart(10, "0")}`,
    conversation_id: "conv-1",
    sender_id: `user-${i % 3}`,
    sender_username: `user${i % 3}`,
    ciphertext: "",
    // Every fourth row carries an attachment and a link preview, the fields that once broke equality.
    content:
      i % 4 === 0
        ? JSON.stringify({
            _att: [{ key: `k${i}`, hash: `h${i}`, kv: 1, sha: "ab", name: "a.png", ct: "image/png", size: 10, w: 4, h: 3 }],
            _txt: `caption ${i}`,
          })
        : `message ${i}`,
    link_preview:
      i % 4 === 0
        ? { url: "https://example.com", title: "Example", image: { key: "pk", hash: "ph", kv: 1, sha: "cd", ct: "image/png", size: 5 } }
        : null,
    sent_at: new Date(1_750_000_000_000 + i * 60_000).toISOString(),
    ...extra,
  };
}

function page(rows: RawChannelMessage[]): MessagePageData {
  return { messages: rows.map(transformChannelMessage), nextCursor: null };
}

function range(from: number, to: number, patch: Record<number, Partial<RawChannelMessage>> = {}) {
  const rows: RawChannelMessage[] = [];
  for (let i = from; i < to; i++) {
    rows.push(raw(i, patch[i]));
  }
  return rows;
}

test("a re-read of the same rows returns the cached page itself", () => {
  const before = page(range(0, 50));
  const shared = shareMessagesById(before, page(range(0, 50)));
  assert.equal(shared, before);
});

test("an arrival that slides the page keeps every surviving row's identity", () => {
  const before = page(range(0, 50));
  const shared = shareMessagesById(before, page(range(1, 51))) as MessagePageData;
  assert.notEqual(shared, before);
  for (const m of shared.messages.slice(0, 49)) {
    assert.equal(m, before.messages.find((b) => b.id === m.id));
  }
  assert.equal(shared.messages[49].id, raw(50).id);
});

// Each change a row can go through; every one must reach the row as a new object.
const CHANGES: Array<[string, Partial<RawChannelMessage>, Partial<RawChannelMessage>]> = [
  ["pending -> failed", { send_state: "pending" }, { send_state: "failed" }],
  ["failed -> pending (retry)", { send_state: "failed" }, { send_state: "pending" }],
  ["pending -> delivered", { send_state: "pending" }, {}],
  ["failed -> rejected", { send_state: "failed" }, { send_state: "rejected" }],
  ["edit fails (edit_unsent)", { edit_state: "pending" }, { edit_state: "failed" }],
  ["edited", {}, { edited_at: "2026-08-01T10:00:00.000Z", content: "edited text" }],
  ["deleted", {}, { deleted_at: "2026-08-01T10:00:00.000Z" }],
  ["undecryptable -> decrypted", { undecryptable: true, content: undefined }, { undecryptable: false }],
  ["content_decrypted changes", {}, { content: "different text" }],
];

for (const [name, from, to] of CHANGES) {
  test(`a same-id row whose state changes (${name}) gets a new object; its neighbours keep theirs`, () => {
    const before = page(range(0, 50, { 10: from }));
    const shared = shareMessagesById(before, page(range(0, 50, { 10: to }))) as MessagePageData;
    assert.notEqual(shared, before);
    assert.notEqual(shared.messages[10], before.messages[10]);
    assert.deepEqual(shared.messages[10], transformChannelMessage(raw(10, to)));
    for (let i = 0; i < 50; i++) {
      if (i !== 10) {
        assert.equal(shared.messages[i], before.messages[i], `row ${i} kept its identity`);
      }
    }
  });
}

test("a failed send stays visible as failed after sharing", () => {
  const before = page(range(0, 5, { 2: { send_state: "pending" } }));
  const shared = shareMessagesById(before, page(range(0, 5, { 2: { send_state: "failed" } }))) as MessagePageData;
  assert.equal(shared.messages[2].status, "failed");
});

test("a non-page value falls back to default sharing", () => {
  const a = [{ id: "x", n: 1 }];
  const same = shareMessagesById(a, [{ id: "x", n: 1 }]);
  assert.equal(same, a);
  const changed = shareMessagesById(a, [{ id: "x", n: 2 }]) as Array<{ n: number }>;
  assert.notEqual(changed, a);
  assert.equal(changed[0].n, 2);
  assert.equal(shareMessagesById(undefined, page(range(0, 2))) !== undefined, true);
});

test("a cursor change alone yields a new page that keeps every row", () => {
  const before = page(range(0, 3));
  const next = { ...page(range(0, 3)), nextCursor: { sent_at: "x", id: "y" } };
  const shared = shareMessagesById(before, next) as MessagePageData;
  assert.notEqual(shared, before);
  assert.deepEqual(shared.nextCursor, { sent_at: "x", id: "y" });
  shared.messages.forEach((m, i) => assert.equal(m, before.messages[i]));
});

test("in development every cached row is frozen, so an in-place write throws", () => {
  const share = shareMessagePage(true);
  const shared = share(undefined, page(range(0, 4))) as MessagePageData;
  const row = shared.messages[0];
  assert.ok(Object.isFrozen(row));
  assert.ok(Object.isFrozen(row.attachments));
  assert.ok(Object.isFrozen(row.attachments![0]));
  assert.ok(Object.isFrozen(row.link_preview));
  assert.ok(Object.isFrozen(row.link_preview!.image));
  assert.throws(() => {
    (row as { status?: string }).status = "failed";
  }, TypeError);
  // Frozen rows still share: a changed row still becomes a new object.
  const again = share(shared, page(range(0, 4, { 1: { send_state: "failed" } }))) as MessagePageData;
  assert.equal(again.messages[0], row);
  assert.equal(again.messages[1].status, "failed");
  assert.equal(freezeMessage(row), row);
});

test("no cache writer mutates a cached message in place", () => {
  // The writers spread into new objects today; this keeps it so (see shareMessagesById).
  const files = ["../src/hooks/queries/useMessages.ts", "../src/components/Layout/MainContent.tsx"];
  for (const file of files) {
    const src = readFileSync(new URL(file, import.meta.url), "utf8");
    assert.doesNotMatch(
      src,
      /\b(old|prev|cached|m|msg|message|row)\.(status|send_state|edited_at|deleted_at|content_decrypted|outbox_id|delivered|undecryptable|attachments|link_preview)\s*=[^=]/,
      `${file} assigns a field of a cached message`,
    );
  }
});
