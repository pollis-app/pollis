// A conditional pointerEvents/opacity View must set collapsable={false}: Fabric churn crashed the PIN pad.
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
