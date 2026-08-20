/*
 * The renderer never writes a user's file to a temp directory.
 *
 * Paste and drag-and-drop arrive in the webview as `File` objects with no
 * filesystem path, and the upload path wanted one — so `ChatInput` wrote the
 * raw source bytes to `tempDir()` as `pollis-<timestamp>-<original filename>`
 * and passed `upload_media` that path. Nothing ever removed it: not the upload,
 * not `removeAttachment` (which revoked the preview blob URL and dropped the
 * array entry), not the send, not app exit. There was not one `removeFile` call
 * in the entire renderer. So the plaintext of every file a user had ever
 * pasted was still sitting in `/tmp` (or `%TEMP%`) under its own name — while
 * `r2.rs` deliberately strips filenames from R2 keys because a filename is
 * content.
 *
 * The fix was not to add deletions on each of the six exit paths. It was to
 * stop making the file: the bytes go to `stageAttachment`, which holds them in
 * the backend's memory, and the only handle the renderer ever sees is an opaque
 * id. A guard is the right shape for that, because the failure it prevents is
 * "somebody writes a temp file again", which no rendering test can see.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

/**
 * Files permitted to call `writeFile`, each with the reason it is not a
 * plaintext leak.
 *
 * The bar: the destination must be a location the USER chose in a save dialog
 * this turn. A path the code picked — anything derived from `tempDir()`, a
 * cache directory, a scratch name — is the shape that leaks, because nothing
 * owns the file afterwards.
 */
const WRITE_FILE_ALLOWED = new Map<string, string>([
  [
    "bridge/fs.ts",
    "the bridge wrapper itself — the narrow surface the rule is stated on",
  ],
  [
    "components/Message/AttachmentDisplay.tsx",
    "'save attachment as…' — writes to the path the user picked in dialogSave",
  ],
]);

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walk(full));
    } else if (/\.(ts|tsx)$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

const FILES = walk(SRC).map((f) => ({
  rel: relative(SRC, f).split("\\").join("/"),
  text: readFileSync(f, "utf8"),
}));

test("no renderer code writes a file into a temp directory", () => {
  const offenders = FILES.filter((f) => /\btempDir\s*\(/.test(f.text)).map(
    (f) => f.rel,
  );

  assert.deepEqual(
    offenders,
    [],
    `these files build a path under the OS temp directory: ${offenders.join(", ")}. ` +
      "A file written there is never deleted by anything in this app. Attachment " +
      "bytes go to stageAttachment() instead, which keeps them in memory.",
  );
});

test("writeFile is only ever aimed at a path the user chose", () => {
  const offenders = FILES.filter(
    (f) => !WRITE_FILE_ALLOWED.has(f.rel) && /\bwriteFile\s*\(/.test(f.text),
  ).map((f) => f.rel);

  assert.deepEqual(
    offenders,
    [],
    `these files call writeFile and are not on the allowlist: ${offenders.join(", ")}. ` +
      "Add the reason to WRITE_FILE_ALLOWED only if the destination is a save-dialog " +
      "path the user picked this turn.",
  );
});

test("a queued attachment's bytes are either a user file or staged, never both and never neither", () => {
  const chatInput = FILES.find(
    (f) => f.rel === "components/ui/ChatInput.tsx",
  );
  assert.ok(chatInput, "ChatInput.tsx must exist");

  // The discriminated union is what makes "a path we invented" unrepresentable:
  // there is no field to put one in.
  assert.match(
    chatInput.text,
    /export type AttachmentSource =\s*\|\s*\{ kind: "path"; path: string \}\s*\|\s*\{ kind: "staged"; id: string \};/,
    "AttachmentSource must stay a two-case discriminated union",
  );
  assert.doesNotMatch(
    chatInput.text,
    /^\s*path: string;\s*$/m,
    "Attachment must not carry a bare `path` field again — that is the shape that " +
      "let a temp-file path and a real user file be the same thing",
  );
});

test("every exit path releases staged bytes", () => {
  const chatInput = FILES.find((f) => f.rel === "components/ui/ChatInput.tsx");
  assert.ok(chatInput);

  // Removing a card, and unmounting while cards are still queued. Send is
  // covered by the backend — a successful upload_media_staged releases its own
  // entry — and lock/logout/wipe release everything.
  const releases = chatInput.text.match(/discardStagedAttachment\(/g) ?? [];
  assert.ok(
    releases.length >= 3,
    `ChatInput releases staged bytes in ${releases.length} place(s); expected at ` +
      "least three (remove, unmount, and the card removed while staging was in flight)",
  );
  assert.match(
    chatInput.text,
    /return \(\) => \{[\s\S]*?discardStagedAttachment/,
    "the unmount cleanup must release whatever is still staged",
  );
});
