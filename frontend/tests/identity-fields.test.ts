/* The rename page for a non-editor (#1281): text, no empty description, no Save, read-only branch first. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { identityFieldMode } from "../src/utils/identityFields.ts";

const here = dirname(fileURLToPath(import.meta.url));
const pageSrc = readFileSync(join(here, "../src/pages/RenameEntity.tsx"), "utf8");

test("an editor always gets an input", () => {
  for (const v of [undefined, null, "", "x"]) {
    assert.equal(identityFieldMode(true, v), "edit");
  }
});

test("a non-editor gets text for a value and nothing for an empty one", () => {
  assert.equal(identityFieldMode(false, "Planning"), "read");
  for (const v of [undefined, null, "", "  "]) {
    assert.equal(identityFieldMode(false, v), "hidden");
  }
});

test("RenameEntity shows a non-admin the read-only view, never the form", () => {
  assert.match(pageSrc, /const isAdmin = group\?\.current_user_role === "admin";/);
  // The read-only branch returns before the form is built.
  const readOnly = pageSrc.indexOf("if (!isAdmin) {\n    return (");
  const form = pageSrc.indexOf("-form`}");
  assert.ok(readOnly > 0 && form > readOnly, "read-only branch must precede the form");
  assert.match(pageSrc, /descriptionMode === "read" && \(/);
  // And a stray submit is refused, not sent.
  assert.match(pageSrc, /setError\(null\);\n    if \(!isAdmin\) \{\n      return;\n    \}/);
});
