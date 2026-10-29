// The OTP boxes must reopen the keyboard after Android back dismissed it.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { reopenKeyboard, type FocusableInput } from "../lib/codeFieldFocus.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

function fakeInput(initiallyFocused: boolean) {
  const calls: string[] = [];
  let focused = initiallyFocused;
  const input: FocusableInput = {
    isFocused: () => focused,
    blur: () => {
      calls.push("blur");
      focused = false;
    },
    focus: () => {
      if (focused) {
        calls.push("focus-ignored");
        return;
      }
      calls.push("focus");
      focused = true;
    },
  };
  return { input, calls };
}

test("still focused after a keyboard dismiss (Android back): blur, then focus reaches native", () => {
  const { input, calls } = fakeInput(true);
  reopenKeyboard(input, false);
  assert.deepEqual(calls, ["blur", "focus"]);
});

test("blurred after a dismiss (iOS): a plain focus", () => {
  const { input, calls } = fakeInput(false);
  reopenKeyboard(input, false);
  assert.deepEqual(calls, ["focus"]);
});

test("keyboard already up: nothing, so it doesn't flicker", () => {
  const { input, calls } = fakeInput(true);
  reopenKeyboard(input, true);
  assert.deepEqual(calls, []);
});

test("no input mounted: no throw", () => {
  reopenKeyboard(null, false);
});

test("the OTP boxes reopen the keyboard through reopenKeyboard, and the hidden input can't eat the tap", () => {
  const src = read("../app/(auth)/otp.tsx");
  assert.match(src, /onPress=\{\(\) => reopenKeyboard\(input\.current, KeyboardController\.isVisible\(\)\)\}/);
  assert.doesNotMatch(src, /input\.current\?\.focus\(\)/);
  const hidden = src.match(/<TextInput[\s\S]*?testID="input-otp"[\s\S]*?\/>/);
  assert.ok(hidden, "otp.tsx renders input-otp");
  assert.match(hidden[0], /pointerEvents: "none"/);
});
