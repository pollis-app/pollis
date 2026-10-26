// The Vault's pure rules (#1316) — each a port of the same rule in desktop's
// frontend/src/pages/Vault.tsx, pinned so the two apps cannot drift.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  editedVaultContent,
  filterVaultEntries,
  isVaultEntryEdited,
  vaultAttachments,
  vaultViewerItems,
  type VaultMessage,
} from "../lib/vault.ts";
import { parseContent } from "../lib/messageContent.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const T0 = "2026-10-01T09:00:00.000Z";
const T1 = "2026-10-02T09:00:00.000Z";

function entry(id: string, content: string, extra: Partial<VaultMessage> = {}): VaultMessage {
  return { id, content, pinned: false, created_at: T0, updated_at: T0, ...extra };
}

function envelope(files: { key: string; name: string; ct: string }[], txt?: string): string {
  return JSON.stringify({
    _att: files.map((f) => ({ key: f.key, url: "", name: f.name, ct: f.ct, size: 10, hash: `h-${f.key}` })),
    ...(txt ? { _txt: txt } : {}),
  });
}

test("search matches the caption and filenames, case-insensitively, and keeps order", () => {
  const entries = [
    entry("a", "Passport number is in the safe"),
    entry("b", envelope([{ key: "media/1", name: "Tax-Return.pdf", ct: "application/pdf" }])),
    entry("c", envelope([{ key: "media/2", name: "dog.jpg", ct: "image/jpeg" }], "the PASSPORT photo")),
  ];
  assert.deepEqual(filterVaultEntries(entries, "passport").map((e) => e.id), ["a", "c"]);
  assert.deepEqual(filterVaultEntries(entries, "  tax-return ").map((e) => e.id), ["b"]);
  assert.deepEqual(filterVaultEntries(entries, "nothing like it"), []);
});

test("a blank search keeps every entry", () => {
  const entries = [entry("a", "one"), entry("b", "two")];
  assert.deepEqual(filterVaultEntries(entries, "   ").map((e) => e.id), ["a", "b"]);
});

test("the media view lists every attachment newest entry first, each object once", () => {
  const entries = [
    entry("old", envelope([{ key: "media/x", name: "x.png", ct: "image/png" }])),
    entry("mid", "text only"),
    entry("new", envelope([
      { key: "media/y", name: "y.png", ct: "image/png" },
      { key: "media/x", name: "x-again.png", ct: "image/png" },
    ])),
  ];
  assert.deepEqual(vaultAttachments(entries).map((a) => a.id), ["media/y", "media/x"]);
});

test("a text edit of a text-only entry saves the trimmed text, and an empty one saves nothing", () => {
  assert.equal(editedVaultContent("old", "  new words \n"), "new words");
  assert.equal(editedVaultContent("old", "   "), null);
});

test("a text edit keeps an entry's files, rebuilding desktop's envelope", () => {
  const original = JSON.stringify({
    _att: [{ key: "media/1", url: "https://x", name: "a.png", ct: "image/png", size: 42, hash: "abc", kv: 0, sha: "d1g", bh: "LKO2", w: 4, h: 3 }],
    _txt: "caption",
  });
  const edited = editedVaultContent(original, " new caption ");
  assert.ok(edited);
  assert.deepEqual(JSON.parse(edited), {
    _att: [{ key: "media/1", url: "", name: "a.png", ct: "image/png", size: 42, hash: "abc", kv: 0, sha: "d1g", bh: "LKO2", w: 4, h: 3 }],
    _txt: "new caption",
  });
  // The rebuilt envelope reads back as the same files.
  assert.deepEqual(
    parseContent(edited, "vault").attachments.map((a) => [a.object_key, a.content_hash, a.filename, a.key_scope]),
    [["media/1", "abc", "a.png", { kind: "vault" }]],
  );
});

test("clearing the caption of an entry with files keeps the files and drops _txt", () => {
  const original = envelope([{ key: "media/1", name: "a.png", ct: "image/png" }], "caption");
  const edited = editedVaultContent(original, "");
  assert.ok(edited);
  const parsed = JSON.parse(edited);
  assert.equal(parsed._att.length, 1);
  assert.equal("_txt" in parsed, false);
});

test("an entry is edited once it was rewritten (pinning counts, as on desktop)", () => {
  assert.equal(isVaultEntryEdited(entry("a", "x")), false);
  assert.equal(isVaultEntryEdited(entry("a", "x", { updated_at: T1 })), true);
});

test("the viewer walks the grid's images and videos in grid order, newest first (#1492)", () => {
  const entries = [
    entry("old", envelope([
      { key: "media/1", name: "1.png", ct: "image/png" },
      { key: "media/2", name: "2.mp4", ct: "video/mp4" },
    ]), { created_at: T0 }),
    entry("doc", envelope([{ key: "media/doc", name: "tax.pdf", ct: "application/pdf" }]), { created_at: T1 }),
    entry("new", envelope([
      { key: "media/3", name: "3.jpg", ct: "image/jpeg" },
      { key: "media/voice", name: "memo.m4a", ct: "audio/mp4" },
      { key: "media/4", name: "4.png", ct: "image/png" },
    ]), { created_at: "2026-10-03T09:00:00.000Z" }),
  ];
  const { items, index } = vaultViewerItems(entries, "media/1");
  // The order the grid lays out (vaultAttachments), minus files and audio.
  assert.deepEqual(items.map((a) => a.id), ["media/3", "media/4", "media/1", "media/2"]);
  assert.equal(index, 2);
  assert.equal(vaultViewerItems(entries, "media/3").index, 0);
});

test("a vault file or voice memo opens alone, and an unknown id yields nothing", () => {
  const entries = [
    entry("a", envelope([
      { key: "media/1", name: "1.png", ct: "image/png" },
      { key: "media/doc", name: "tax.pdf", ct: "application/pdf" },
    ])),
  ];
  assert.deepEqual(vaultViewerItems(entries, "media/doc").items.map((a) => a.id), ["media/doc"]);
  assert.deepEqual(vaultViewerItems(entries, "media/gone"), { items: [], index: 0 });
});

test("hundreds of vault items page in grid order around the tapped one", () => {
  const entries = Array.from({ length: 300 }, (_, i) =>
    entry(`e${i}`, envelope([{ key: `media/${i}`, name: `${i}.png`, ct: "image/png" }]), {
      created_at: new Date(Date.UTC(2026, 0, 1, 0, i)).toISOString(),
    }),
  );
  const { items, index } = vaultViewerItems(entries, "media/150");
  assert.equal(items.length, 300);
  assert.equal(items[0].id, "media/299");
  assert.equal(items[index].id, "media/150");
});

test("the screen is reachable from Direct and from Search, and registered on the root stack", () => {
  assert.match(src("app/(tabs)/direct.tsx"), /<VaultRow onPress=\{\(\) => router\.push\("\/vault"\)\}/);
  assert.match(src("app/(tabs)/search.tsx"), /to: "\/vault" as const/);
  assert.match(src("app/_layout.tsx"), /<Stack\.Screen name="vault" \/>/);
});

test("the media viewer reads the vault only when opened from it", () => {
  const viewer = src("app/media.tsx");
  assert.match(viewer, /useVaultMessages\(fromVault\)/);
  assert.match(viewer, /vaultViewerItems\(/);
  assert.match(viewer, /params\.source === "vault"/);
});

test("delete asks first: the sheet arms the bar, with the confirm haptic", () => {
  const screen = src("app/vault.tsx");
  assert.match(screen, /haptic\("confirmArm"\);\s*setPendingDeleteId\(entry\.id\)/);
  assert.match(screen, /<VaultDeleteBar/);
});
