/*
 * The soft keyboard is handled by react-native-keyboard-controller (#1246).
 *
 * The app is edge-to-edge, so Android's `adjustResize` is inert, React
 * Native's own `KeyboardAvoidingView` misses a keyboard that was already up
 * when a screen mounted, and an inset hand-rolled from `keyboardDidShow` came
 * out short on gesture-nav phones with a keyboard toolbar, cutting off the
 * chat composer. None of that shows up in Maestro's view hierarchy, so the
 * wiring is pinned here instead.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

// Every .ts/.tsx source under the app's own directories.
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

// #1432: `automaticOffset` takes the frame's height from the layout event but
// its window position from an async native call, which on Android can answer
// before the screen is mounted below the status bar (y=0). Nothing measures
// again, so every keyboard on that screen stopped a status bar short and hid
// the chat composer behind it. <Screen> is always a full screen whose top is
// the window's top, so the layout event's own frame (relative to the
// SafeAreaView) is exact and cannot go stale. The sheet keeps it: its view
// fills the Modal's window from the first layout, so the position is 0 either
// way.
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
