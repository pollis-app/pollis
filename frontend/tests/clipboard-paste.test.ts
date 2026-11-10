// The composer's paste path (#1794): a Steam screenshot paste on X11 froze the window.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  createOsPasteReader,
  pastedFiles,
  type OsPasteDeps,
  type PasteItem,
} from "../src/utils/clipboardPaste.ts";

const fileItem = (file: File | null): PasteItem => ({ kind: "file", getAsFile: () => file });
const stringItem: PasteItem = { kind: "string", getAsFile: () => null };

test("files on the paste event are taken as they are", () => {
  const png = new File([new Uint8Array(8)], "a.png", { type: "image/png" });
  const bmp = new File([new Uint8Array(54)], "b.bmp", { type: "image/bmp" });
  assert.deepEqual(pastedFiles([stringItem, fileItem(png), fileItem(bmp)]), [png, bmp]);
});

test("an empty or unreadable file item is not a file, so the OS read still runs", () => {
  const empty = new File([], "pasted.png", { type: "image/png" });
  assert.deepEqual(pastedFiles([fileItem(empty), fileItem(null)]), []);
  // A uri-list-only paste (what Steam offers) carries no file item at all.
  assert.deepEqual(pastedFiles([stringItem]), []);
  assert.deepEqual(pastedFiles([]), []);
});

test("a large file passes through untouched: no decode or resize on the paste", () => {
  const big = new File([new Uint8Array(40 * 1024 * 1024)], "4k.bmp", { type: "image/bmp" });
  assert.deepEqual(pastedFiles([fileItem(big)]), [big]);
});

function deps(over: Partial<OsPasteDeps> = {}) {
  const calls = { paths: [] as string[][], files: [] as File[], images: 0 };
  const d: OsPasteDeps = {
    readFiles: async () => [],
    readImage: async () => {
      calls.images += 1;
      return null;
    },
    onPaths: (p) => calls.paths.push(p),
    onFile: (f) => calls.files.push(f),
    ...over,
  };
  return { d, calls };
}

test("a Steam copy (a file path) attaches the path and never reads an image", async () => {
  const { d, calls } = deps({ readFiles: async () => ["/home/someone/.steam/shot.jpg"] });
  assert.equal(await createOsPasteReader(() => d)(), "paths");
  assert.deepEqual(calls.paths, [["/home/someone/.steam/shot.jpg"]]);
  assert.equal(calls.images, 0);
});

test("no files: the clipboard image becomes a pasted PNG", async () => {
  const bytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
  const { d, calls } = deps({ readImage: async () => bytes });
  assert.equal(await createOsPasteReader(() => d)(), "image");
  assert.equal(calls.files.length, 1);
  assert.equal(calls.files[0].name, "pasted-image.png");
  assert.equal(calls.files[0].type, "image/png");
  assert.equal(calls.files[0].size, 4);
});

test("a clipboard that errors or holds nothing is a quiet no-op", async () => {
  const failing = deps({
    readFiles: async () => { throw new Error("clipboard unreadable"); },
    readImage: async () => { throw new Error("no image"); },
  });
  assert.equal(await createOsPasteReader(() => failing.d)(), "none");
  const empty = deps({ readImage: async () => new Uint8Array() });
  assert.equal(await createOsPasteReader(() => empty.d)(), "none");
  assert.equal(empty.calls.files.length, 0);
});

test("a second paste while a read is pending is dropped, not queued behind it", async () => {
  let release!: (paths: string[]) => void;
  let reads = 0;
  const { d, calls } = deps({
    readFiles: () => {
      reads += 1;
      return new Promise<string[]>((resolve) => { release = resolve; });
    },
  });
  const read = createOsPasteReader(() => d);
  const first = read();
  assert.equal(await read(), "busy");
  assert.equal(await read(), "busy");
  release(["/tmp/a.png"]);
  assert.equal(await first, "paths");
  assert.equal(reads, 1);
  assert.deepEqual(calls.paths, [["/tmp/a.png"]]);
  // Settled: the next paste reads again.
  const next = read();
  release([]);
  assert.equal(await next, "none");
  assert.equal(reads, 2);
});

test("results go to the composer's latest handlers, not the ones from when the read began", async () => {
  let release!: (paths: string[]) => void;
  const seen: string[] = [];
  let current: OsPasteDeps = {
    readFiles: () => new Promise<string[]>((resolve) => { release = resolve; }),
    readImage: async () => null,
    onPaths: () => seen.push("stale"),
    onFile: () => {},
  };
  const pending = createOsPasteReader(() => current)();
  current = { ...current, onPaths: () => seen.push("fresh") };
  release(["/tmp/a.png"]);
  await pending;
  assert.deepEqual(seen, ["fresh"]);
});

test("every clipboard command runs off the main thread (async), never a sync command", () => {
  const lib = readFileSync(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8");
  for (const name of ["read_clipboard_files", "read_clipboard_image", "write_clipboard_text"]) {
    assert.match(lib, new RegExp(`async fn ${name}\\(`), `${name} must be async`);
    assert.doesNotMatch(lib, new RegExp(`\\n\\s*fn ${name}\\(`), `${name} must not be sync`);
  }
});
