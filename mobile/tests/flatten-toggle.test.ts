// A View whose pointerEvents (or opacity) is conditional flips between
// flattened and unflattened in Fabric unless it also forms a view for
// another reason. On Android that churn crashed the PIN keypad
// ("Unable to find viewState for tag N" in updatePadding). Such views must
// opt out of flattening with collapsable={false}.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

test("PIN keypad container is never flattened", () => {
  const src = readFileSync(join(import.meta.dirname, "..", "components/auth/PinPad.tsx"), "utf8");
  const keypad = src.slice(src.indexOf("export function PinKeypad"));
  const openTag = keypad.slice(keypad.indexOf("<View"), keypad.indexOf(">", keypad.indexOf("pointerEvents=")) + 1);
  assert.match(openTag, /collapsable=\{false\}/);
});
