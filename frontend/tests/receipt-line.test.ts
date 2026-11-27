/*
 * The desktop DM status line's words (#2140): "Delivered", then "Read <time>", never a state
 * before a peer's device has the message (#892).
 */

import test from "node:test";
import assert from "node:assert/strict";

import { receiptLineText } from "../src/utils/receiptLine.ts";

const t = (key: string, opts?: Record<string, unknown>) =>
  opts ? `${key}:${JSON.stringify(opts)}` : key;
const fmt = (at: string) => `@${at}`;

test("nothing until a peer's device has it", () => {
  assert.equal(receiptLineText(undefined, 1, t, fmt), null);
  assert.equal(receiptLineText({ delivered_by: [], read_by: [] }, 1, t, fmt), null);
});

test("delivered, then read at the reader's own time", () => {
  assert.deepEqual(receiptLineText({ delivered_by: ["bob"], read_by: [] }, 1, t, fmt), {
    state: "delivered",
    text: "chat:receipts.delivered",
  });
  assert.deepEqual(
    receiptLineText(
      { delivered_by: ["bob"], read_by: ["bob"], read_at: { bob: "2026-11-01T11:03:00Z" } },
      1,
      t,
      fmt,
    ),
    { state: "read", text: `chat:receipts.readAt:${JSON.stringify({ time: "@2026-11-01T11:03:00Z" })}` },
  );
});

test("a read with no time still reads as read", () => {
  assert.deepEqual(receiptLineText({ delivered_by: ["bob"], read_by: ["bob"] }, 1, t, fmt), {
    state: "read",
    text: "chat:receipts.read",
  });
});

test("a larger DM counts readers until all have read, then shows the last one's time", () => {
  const some = receiptLineText(
    { delivered_by: ["bob", "carol", "dave"], read_by: ["bob"], read_at: { bob: "2026-11-01T11:00:00Z" } },
    3,
    t,
    fmt,
  );
  assert.equal(some?.state, "read-some");
  assert.equal(some?.text, `chat:receipts.readByCount:${JSON.stringify({ count: 1, total: 3 })}`);

  const all = receiptLineText(
    {
      delivered_by: ["bob", "carol"],
      read_by: ["bob", "carol"],
      read_at: { bob: "2026-11-01T11:05:00Z", carol: "2026-11-01T11:01:00Z" },
    },
    2,
    t,
    fmt,
  );
  assert.equal(all?.text, `chat:receipts.readAt:${JSON.stringify({ time: "@2026-11-01T11:05:00Z" })}`);
});
