/*
 * A desktop preferences save keeps the keys desktop does not know (#1650).
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  mergePreferencesSave,
  parsePreferencesBlob,
  preferencesSaveJson,
} from "../src/utils/preferencesBlob.ts";

const stored = JSON.stringify({
  accent_color: "#fabf5a",
  accent_hex: "#5ab0fa",
  mobile_behavior: { haptics: false },
  font_size: "16",
});

test("a desktop save sends mobile's keys back unchanged", () => {
  const raw = parsePreferencesBlob(stored);
  // What desktop's settings pages pass: only the keys it parses.
  const merged = mergePreferencesSave(raw, { accent_color: "#ff0000", menubar_icon: true });
  const sent = JSON.parse(preferencesSaveJson(merged));
  assert.equal(sent.accent_hex, "#5ab0fa");
  assert.deepEqual(sent.mobile_behavior, { haptics: false });
  assert.equal(sent.accent_color, "#ff0000");
  assert.equal(sent.menubar_icon, true);
});

test("a key set to undefined is deleted, and sent as null so the DS deletes it too", () => {
  const raw = parsePreferencesBlob(stored);
  const merged = mergePreferencesSave(raw, { font_size: undefined, user_volumes: undefined });
  assert.equal("font_size" in merged.blob, false);
  // `user_volumes` was never stored, so there is nothing to delete.
  assert.deepEqual([...merged.deleted], ["font_size"]);
  const sent = JSON.parse(preferencesSaveJson(merged));
  assert.equal(sent.font_size, null);
  assert.equal(sent.accent_hex, "#5ab0fa");
});

test("a delete survives a coalesced save that does not name the key", () => {
  const first = mergePreferencesSave(parsePreferencesBlob(stored), { font_size: undefined });
  const second = mergePreferencesSave(first.blob, { menubar_icon: false }, first.deleted);
  assert.equal(JSON.parse(preferencesSaveJson(second)).font_size, null);

  // Setting the key again cancels the pending delete.
  const third = mergePreferencesSave(second.blob, { font_size: "18" }, second.deleted);
  assert.equal(third.deleted.size, 0);
  assert.equal(JSON.parse(preferencesSaveJson(third)).font_size, "18");
});

test("anything but a JSON object parses as empty", () => {
  for (const bad of ["", "[]", "42", "null", "{nope", undefined, null]) {
    assert.deepEqual(parsePreferencesBlob(bad), {});
  }
});
