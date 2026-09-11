/*
 * The on-device export (#856) is an exit, not a backup channel.
 *
 * The constraint on the ticket is that the archive must never grow a re-import
 * path: a reader for the file is exactly the history-sync mechanism the
 * project refuses to build. `pollis-core` guards its own half in
 * `commands/export.rs`; this is the renderer's half — no `invoke()` anywhere
 * in `frontend/src` may name an import/restore command, and the only caller of
 * `export_archive` is the typed wrapper in `services/api.ts`.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { resolve, dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = resolve(dirname(fileURLToPath(import.meta.url)), "../src");

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...sourceFiles(full));
    } else if (/\.tsx?$/.test(entry)) {
      out.push(full);
    }
  }
  return out;
}

test("no renderer code invokes an archive import or restore command", () => {
  const offenders: string[] = [];
  for (const file of sourceFiles(SRC)) {
    const text = readFileSync(file, "utf8");
    if (/invoke[<(][^)]*['"](import|restore)_(archive|account|history)/.test(text)) {
      offenders.push(relative(SRC, file));
    }
  }
  assert.deepEqual(offenders, [], "an archive re-import path was added; #856 forbids one");
});

test("export_archive is invoked only through services/api.ts", () => {
  const callers = sourceFiles(SRC)
    .filter((file) => /['"]export_archive['"]/.test(readFileSync(file, "utf8")))
    .map((file) => relative(SRC, file));
  assert.deepEqual(callers, ["services/api.ts"]);
});
