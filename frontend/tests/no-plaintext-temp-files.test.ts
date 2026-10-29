/* The renderer never writes a user's file to a temp directory: pasted bytes go to `stageAttachment`, in backend memory. */

import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

/** Files allowed to call `writeFile`; empty, since saving moved to Rust's `save_media_to_path`. */
const WRITE_FILE_ALLOWED = new Map<string, string>([]);

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

  // Card removal and unmount; sends and lock/logout/wipe are released by the backend.
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
