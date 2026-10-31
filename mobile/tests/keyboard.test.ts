/* The soft keyboard is handled by react-native-keyboard-controller (#1246); Maestro cannot see the wiring. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

function sources(): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (statSync(full).isDirectory()) {
        walk(full);
      } else if (/\.tsx?$/.test(name)) {
        out.push(relative(ROOT, full));
      }
    }
  };
  for (const dir of ["app", "components", "hooks", "lib"]) {
    walk(join(ROOT, dir));
  }
  return out;
}

test("the root layout mounts the KeyboardProvider", () => {
  const layout = read("app/_layout.tsx");
  assert.match(layout, /from "react-native-keyboard-controller"/);
  assert.match(layout, /<KeyboardProvider>/);
});

test("<Screen> and the sheet avoid the keyboard with keyboard-controller on both platforms", () => {
  for (const file of ["components/ui.tsx", "components/chat/SheetOverlay.tsx"]) {
    const src = read(file);
    assert.match(
      src,
      /import \{ KeyboardAvoidingView \} from "react-native-keyboard-controller";/,
      `${file} must take KeyboardAvoidingView from keyboard-controller`,
    );
    const kav = src.match(/<KeyboardAvoidingView[\s\S]*?>/);
    assert.ok(kav, `${file} must render a KeyboardAvoidingView`);
    assert.match(kav[0], /behavior="padding"/, `${file}: one behaviour for iOS and Android`);
  }
});

// #1432: `automaticOffset`'s async window position can be stale on Android; the sheet keeps it (its position is always 0).
test("<Screen> avoids the keyboard from its layout frame, not automaticOffset", () => {
  const kav = read("components/ui.tsx").match(/<KeyboardAvoidingView[\s\S]*?>/);
  assert.ok(kav);
  assert.doesNotMatch(kav[0], /automaticOffset/, "<Screen>: no async window position (#1432)");
  const sheet = read("components/chat/SheetOverlay.tsx").match(/<KeyboardAvoidingView[\s\S]*?>/);
  assert.ok(sheet);
  assert.match(sheet[0], /automaticOffset/, "the sheet measures its place in the Modal's window");
});

test("nothing uses React Native's KeyboardAvoidingView or hand-rolls a keyboard inset", () => {
  for (const file of sources()) {
    const src = read(file);
    const rnImport = src.match(/import\s*\{([^}]*)\}\s*from\s*"react-native";/g) ?? [];
    for (const imp of rnImport) {
      assert.doesNotMatch(imp, /\bKeyboardAvoidingView\b/, `${file} imports RN's KeyboardAvoidingView`);
    }
    assert.doesNotMatch(
      src,
      /Keyboard\.addListener\(\s*"keyboard(Did|Will)(Show|Hide)"/,
      `${file} hand-rolls a keyboard inset from Keyboard events`,
    );
  }
});

test("react-native-keyboard-controller is pinned to the Expo SDK's version", () => {
  const pkg = JSON.parse(read("package.json"));
  const bundled = JSON.parse(read("node_modules/expo/bundledNativeModules.json"));
  assert.equal(
    pkg.dependencies["react-native-keyboard-controller"],
    bundled["react-native-keyboard-controller"],
  );
});

// #1641/#1642: Android hides the keyboard without an animation when the window is backgrounded or loses
// focus, so keyboard-controller 1.21.9 never sent the hide and every <Screen> stayed padded at the
// keyboard's height. The pnpm patch replays such a hide; it must stay registered and applied.
test("keyboard-controller is patched to replay a hide Android applied without an animation", () => {
  const pkg = JSON.parse(read("package.json"));
  const version = pkg.dependencies["react-native-keyboard-controller"];
  const key = `react-native-keyboard-controller@${version}`;
  const patchPath = pkg.pnpm.patchedDependencies[key];
  assert.equal(
    patchPath,
    `patches/${key}.patch`,
    "a keyboard-controller bump must carry the #1641 patch forward (or drop it once upstream replays a missed hide)",
  );
  assert.match(read(patchPath), /reconcileMissedHide/);
  const callback = read(
    "node_modules/react-native-keyboard-controller/android/src/main/java/com/reactnativekeyboardcontroller/listeners/KeyboardAnimationCallback.kt",
  );
  assert.match(callback, /private fun reconcileMissedHide\(/, "the installed copy must carry the patch");
  assert.match(callback, /addOnWindowFocusChangeListener\(windowFocusListener\)/, "regaining window focus re-checks");
  assert.match(callback, /postDelayed\(missedHideCheck, MISSED_HIDE_GRACE_MS\)/, "an insets change re-checks");
});

test("<Screen> draws the keyboard-state e2e probe", () => {
  assert.match(read("components/ui.tsx"), /<KeyboardProbe \/>/);
  assert.match(read("components/KeyboardProbe.tsx"), /"keyboard-open" : "keyboard-closed"/);
});
