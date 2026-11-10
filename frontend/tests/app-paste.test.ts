// The app's own paste on X11 (#1794): WebKitGTK's paste hung on Steam's clipboard.

import { test } from "node:test";
import assert from "node:assert/strict";
import { createAppPaste, isPasteChord, type AppPasteDeps, type PasteKey } from "../src/utils/appPaste.ts";

function key(over: Partial<PasteKey>): PasteKey & { prevented: boolean } {
  const k = {
    key: "", code: "", ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, prevented: false,
    preventDefault() { k.prevented = true; },
    ...over,
  };
  return k;
}

type Target = { id: string };

function deps(over: Partial<AppPasteDeps<Target>> = {}) {
  const delivered: Array<[string, string]> = [];
  const d: AppPasteDeps<Target> = {
    readText: async () => "hello",
    focused: () => ({ id: "composer" }),
    isCurrent: () => true,
    deliver: (t, text) => delivered.push([t.id, text]),
    ...over,
  };
  return { d, delivered };
}

test("Ctrl+V, Ctrl+Shift+V and Shift+Insert are paste chords; others are not", () => {
  assert.ok(isPasteChord(key({ key: "v", code: "KeyV", ctrlKey: true })));
  assert.ok(isPasteChord(key({ key: "V", code: "KeyV", ctrlKey: true, shiftKey: true })));
  assert.ok(isPasteChord(key({ key: "Insert", code: "Insert", shiftKey: true })));
  // A non-Latin layout still has the V key's code.
  assert.ok(isPasteChord(key({ key: "м", code: "KeyV", ctrlKey: true })));
  assert.ok(!isPasteChord(key({ key: "v", code: "KeyV" })));
  assert.ok(!isPasteChord(key({ key: "v", code: "KeyV", ctrlKey: true, altKey: true })));
  assert.ok(!isPasteChord(key({ key: "c", code: "KeyC", ctrlKey: true })));
  assert.ok(!isPasteChord(key({ key: "Insert", code: "Insert", ctrlKey: true })));
});

test("a paste chord never reaches the webview, and the text goes to the focused field", async () => {
  const { d, delivered } = deps();
  const app = createAppPaste(d);
  const k = key({ key: "v", code: "KeyV", ctrlKey: true });
  app.onKeyDown(k);
  assert.ok(k.prevented);
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(delivered, [["composer", "hello"]]);
});

test("other keys pass through untouched", () => {
  const { d } = deps({ readText: async () => assert.fail("no read for a non-paste key") });
  const k = key({ key: "a", code: "KeyA", ctrlKey: true });
  createAppPaste(d).onKeyDown(k);
  assert.ok(!k.prevented);
});

test("no text (a Steam copy) still delivers a paste, so the composer reads files", async () => {
  const { d, delivered } = deps({ readText: async () => null });
  assert.equal(await createAppPaste(d).paste(), "pasted");
  assert.deepEqual(delivered, [["composer", ""]]);
});

test("a failing read is an empty paste, not an error", async () => {
  const { d, delivered } = deps({ readText: async () => { throw new Error("owner gone"); } });
  assert.equal(await createAppPaste(d).paste(), "pasted");
  assert.deepEqual(delivered, [["composer", ""]]);
});

test("no editable focus: the chord is swallowed and nothing is read", async () => {
  let reads = 0;
  const { d, delivered } = deps({ focused: () => null, readText: async () => { reads++; return "x"; } });
  const app = createAppPaste(d);
  const k = key({ key: "v", code: "KeyV", ctrlKey: true });
  app.onKeyDown(k);
  assert.ok(k.prevented);
  assert.equal(await app.paste(), "no-target");
  assert.equal(reads, 0);
  assert.deepEqual(delivered, []);
});

test("one read at a time: repeats while a slow owner answers are dropped", async () => {
  let release!: (t: string) => void;
  let reads = 0;
  const { d, delivered } = deps({ readText: () => { reads++; return new Promise((r) => { release = r; }); } });
  const app = createAppPaste(d);
  const first = app.paste();
  assert.equal(await app.paste(), "busy");
  assert.equal(await app.paste(), "busy");
  release("late");
  assert.equal(await first, "pasted");
  assert.equal(reads, 1);
  assert.deepEqual(delivered, [["composer", "late"]]);
  // Settled: the next paste reads again.
  const next = app.paste();
  release("again");
  assert.equal(await next, "pasted");
  assert.equal(reads, 2);
});

test("focus moved on while the read waited: the text is not pasted somewhere else", async () => {
  const { d, delivered } = deps({ isCurrent: () => false });
  assert.equal(await createAppPaste(d).paste(), "stale");
  assert.deepEqual(delivered, []);
});
