/*
 * The rename page for someone who cannot edit (#1281): name and description
 * as text, an empty description not shown at all, no Save.
 *
 * `src/utils/identityFields.ts` holds the rule (mobile/tests/identity-fields
 * pins its mobile mirror equal); this also pins that RenameEntity routes a
 * non-admin to the read-only view instead of the form.
 *
 *   node --test frontend/tests/
 */
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
