/* Group settings for a non-admin (#1281): text, not inputs; an empty description is not shown. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { identityFieldMode, isGroupAdmin } from "../lib/identityFields.ts";

const here = dirname(fileURLToPath(import.meta.url));
const settingsSrc = readFileSync(join(here, "../app/group/settings.tsx"), "utf8");

const VALUES = [undefined, null, "", "   ", "\n", "Book club", "  padded  "];

test("an admin always gets an input, whatever the value", () => {
  for (const v of VALUES) {
    assert.equal(identityFieldMode(true, v), "edit");
  }
});

test("a non-admin sees an empty value as nothing", () => {
  for (const v of [undefined, null, "", "   ", "\n"]) {
    assert.equal(identityFieldMode(false, v), "hidden");
  }
});

test("a non-admin sees a real value as read-only text", () => {
  assert.equal(identityFieldMode(false, "Book club"), "read");
  assert.equal(identityFieldMode(false, "  padded  "), "read");
});

test("desktop's copy declares the same rule", () => {
  const fnOf = (src: string) => {
    const m = src.match(/export function identityFieldMode\([^]*?\n}\n/);
    assert.ok(m, "identityFieldMode must be declared");
    return m[0];
  };
  const mobile = readFileSync(join(here, "../lib/identityFields.ts"), "utf8");
  const desktop = readFileSync(
    join(here, "../../frontend/src/utils/identityFields.ts"),
    "utf8",
  );
  assert.equal(fnOf(desktop), fnOf(mobile));
});

test("admin is read from the roster first, then the group row", () => {
  assert.equal(isGroupAdmin("admin", "member"), true);
  assert.equal(isGroupAdmin("owner", undefined), true);
  assert.equal(isGroupAdmin("member", "admin"), false);
  assert.equal(isGroupAdmin(undefined, "admin"), true);
  assert.equal(isGroupAdmin(undefined, "member"), false);
  assert.equal(isGroupAdmin(undefined, undefined), false);
});

test("group settings renders inputs only in edit mode", () => {
  // Inputs are mounted only for an editor.
  assert.doesNotMatch(settingsSrc, /editable=\{iAmAdmin\}/);
  assert.match(settingsSrc, /identityFieldMode\(iAmAdmin, group\?\.description\)/);
  assert.match(settingsSrc, /descriptionMode === "edit" \? \(\s*<LabeledField[^]*?testID="input-group-description"/);
  assert.match(settingsSrc, /descriptionMode === "read" \? \([^]*?testID="value-group-description"/);
  assert.match(settingsSrc, /nameMode === "read" \? \([^]*?testID="value-group-name"/);
});
