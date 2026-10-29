/* The export (#856) never grows a re-import path; only `services/api.ts` invokes `export_archive`. */

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
    // The Playwright IPC mock answers these commands; it is not a caller.
    if (entry === "__mocks__") {
      continue;
    }
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

for (const command of ["export_archive", "fetch_export_attachments"]) {
  test(`${command} is invoked only through services/api.ts`, () => {
    const callers = sourceFiles(SRC)
      .filter((file) => new RegExp(`['"]${command}['"]`).test(readFileSync(file, "utf8")))
      .map((file) => relative(SRC, file));
    assert.deepEqual(callers, ["services/api.ts"]);
  });
}

test("the network step of the export is only ever reachable from its own opt-in button", () => {
  const callers = sourceFiles(SRC)
    .filter((file) => /fetchExportAttachments\(/.test(readFileSync(file, "utf8")))
    .map((file) => relative(SRC, file));
  assert.deepEqual(callers, ["components/Security/ExportArchiveButton.tsx", "services/api.ts"]);
});
