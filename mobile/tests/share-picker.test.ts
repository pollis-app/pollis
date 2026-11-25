// The share picker's rules (#2100): lib/share/shareRules.ts.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  afterSend,
  canSend,
  initialMessage,
  itemKind,
  MAX_BYTES,
  MAX_ITEMS,
  MAX_TARGETS,
  parseManifest,
  shareRequest,
  toggleTarget,
} from "../lib/share/shareRules.ts";

test("the limits match the core's", () => {
  assert.equal(MAX_TARGETS, 5);
  assert.equal(MAX_ITEMS, 10);
  assert.equal(MAX_BYTES, 100 * 1024 * 1024);
});

test("a sixth chat is refused and changes nothing; unselecting always works", () => {
  let sel: string[] = [];
  for (const id of ["vault", "a", "b", "c", "d"]) {
    const next = toggleTarget(sel, id);
    assert.equal(next.capped, false);
    sel = next.selected;
  }
  assert.deepEqual(sel, ["vault", "a", "b", "c", "d"], "the Vault counts like any chat");
  const sixth = toggleTarget(sel, "e");
  assert.equal(sixth.capped, true);
  assert.deepEqual(sixth.selected, sel);
  const off = toggleTarget(sel, "b");
  assert.deepEqual(off.selected, ["vault", "a", "c", "d"]);
  assert.equal(off.capped, false);
});

test("send needs a chat and something to send", () => {
  const draft = parseManifest(
    JSON.stringify({ id: "s1", items: [{ path: "/c/1-a.jpg", name: "a.jpg", mimeType: "image/jpeg", size: 10 }] }),
  );
  assert.equal(canSend([], "hi", null), false);
  assert.equal(canSend(["a"], "  ", null), false);
  assert.equal(canSend(["a"], "hi", null), true);
  assert.equal(canSend(["a"], "", draft), true, "files alone are enough");
  assert.equal(canSend(["a", "b", "c", "d", "e", "f"], "hi", null), false);
});

test("the shared link pre-fills the message; a page title only when there is no text", () => {
  assert.equal(initialMessage("https://example.com/a", "A page"), "https://example.com/a");
  assert.equal(initialMessage("  ", "A page"), "A page");
  assert.equal(initialMessage(undefined, undefined), "");
  const draft = parseManifest(JSON.stringify({ id: "s", text: " look https://x.org ", items: [] }));
  assert.equal(draft?.message, "look https://x.org");
});

test("the manifest is read defensively: caps, refusals and junk", () => {
  assert.equal(parseManifest(null), null);
  assert.equal(parseManifest("not json"), null);
  assert.equal(parseManifest(JSON.stringify({ items: [] })), null, "no id");
  const items = Array.from({ length: 12 }, (_, i) => ({
    path: `/c/${i}`,
    name: `f${i}.mp4`,
    mimeType: "video/mp4",
    size: 1,
  }));
  items.push({ path: "/c/big", name: "big.bin", mimeType: "application/zip", size: MAX_BYTES + 1 });
  const d = parseManifest(
    JSON.stringify({
      id: "s2",
      items: [...items, { nope: true }],
      refused: [{ name: "huge.mov", mimeType: "video/quicktime", reason: "too_large" }, { name: "x", reason: "unreadable" }],
      truncated: false,
    }),
  );
  assert.ok(d);
  assert.equal(d.items.length, MAX_ITEMS);
  assert.ok(d.items.every((i) => i.kind === "video"));
  assert.equal(d.truncated, true, "more than ten were shared");
  assert.deepEqual(d.tooLarge, ["huge.mov"]);
});

test("item kinds follow the MIME type", () => {
  assert.equal(itemKind("image/heic"), "image");
  assert.equal(itemKind("video/mp4"), "video");
  assert.equal(itemKind("application/pdf"), "file");
  assert.equal(itemKind("text/plain"), "file");
});

test("one chat opens it, the Vault opens the Vault, several go back", () => {
  assert.deepEqual(afterSend([{ conversationId: "d1", kind: "dm" }]), { to: "chat", conversationId: "d1", kind: "dm" });
  assert.deepEqual(afterSend([{ conversationId: "vault", kind: "vault" }]), { to: "vault" });
  assert.deepEqual(
    afterSend([
      { conversationId: "d1", kind: "dm" },
      { conversationId: "c1", kind: "channel" },
    ]),
    { to: "back" },
  );
});

test("the request is what share_enqueue takes, files by path with any video poster", () => {
  const draft = parseManifest(
    JSON.stringify({
      id: "s3",
      items: [
        { path: "/c/1-a.jpg", name: "a.jpg", mimeType: "image/jpeg", size: 3 },
        { path: "/c/2-b.mp4", name: "b.mp4", mimeType: "video/mp4", size: 3 },
      ],
    }),
  );
  const req = shareRequest("01SHARE", ["vault", "d1"], "  hi  ", draft, { "/c/2-b.mp4": "POSTER" });
  assert.deepEqual(req, {
    shareId: "01SHARE",
    targets: ["vault", "d1"],
    text: "hi",
    attachments: [
      { path: "/c/1-a.jpg", filename: "a.jpg", contentType: "image/jpeg" },
      { path: "/c/2-b.mp4", filename: "b.mp4", contentType: "video/mp4", poster: "POSTER" },
    ],
  });
});
