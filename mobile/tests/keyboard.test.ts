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
// keyboard's height. iOS strands the same way after a background/resume or a stray KVO move.
// The pnpm patch replays such a hide on both platforms; it must stay registered and applied.
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

  const ios = "node_modules/react-native-keyboard-controller/ios/observers/movement/observer/";
  assert.match(
    read(`${ios}KeyboardMovementObserver+Lifecycle.swift`),
    /UIApplication\.didBecomeActiveNotification/,
    "iOS re-checks when the app returns to the foreground",
  );
  assert.match(
    read(`${ios}KeyboardMovementObserver+Lifecycle.swift`),
    /UIWindow\.didBecomeKeyNotification/,
    "iOS re-checks when its window becomes key again",
  );
  const listeners = read(`${ios}KeyboardMovementObserver+Listeners.swift`);
  assert.match(listeners, /func reconcileHiddenKeyboard\(_ reason: String\) -> Bool/, "iOS replays a hide JS never saw");
  // A foreground return resends the hide even when JS was last told 0: events dispatched while
  // backgrounded can be lost (#1641 coordinator run), so "JS holds 0" proves nothing there.
  assert.match(listeners, /scheduleReconcile\("active", force: true\)/);
  assert.match(listeners, /scheduleReconcile\("didHide", force: false\)/);
  // Bounded re-check after a signal, never a timer or a single fixed delay.
  assert.match(listeners, /for delay in \[0\.0, 0\.1, 0\.25, 0\.5, 1\.0, 2\.0\]/);
  assert.doesNotMatch(listeners, /Timer\.scheduledTimer|repeats: true/);
  assert.match(
    read(`${ios}KeyboardMovementObserver+Interactive.swift`),
    /!isKeyboardShown \|\| UIApplication\.shared\.applicationState != \.active/,
    "iOS drops KVO moves of a hidden keyboard or a backgrounded app",
  );
});

test("<Screen> draws the keyboard-state e2e probe", () => {
  assert.match(read("components/ui.tsx"), /<KeyboardProbe \/>/);
  assert.match(read("components/KeyboardProbe.tsx"), /"keyboard-open" : "keyboard-closed"/);
});

// #1642: a screen whose only input is the PIN keypad never shows the system keyboard, so it must not avoid one.
test("every PIN-keypad screen, and only those, opts out of keyboard avoidance", () => {
  const blocks: { file: string; block: string }[] = [];
  for (const file of sources().filter((f) => f.startsWith("app/"))) {
    for (const m of read(file).matchAll(/<Screen\b[\s\S]*?<\/Screen>/g)) {
      blocks.push({ file, block: m[0] });
    }
  }
  const optedOut = blocks.filter((b) => /<Screen\b[^>]*avoidKeyboard=\{false\}/.test(b.block));
  const keypad = blocks.filter((b) => /<PinKeypad\b|\{keypad\}/.test(b.block));
  assert.deepEqual(
    optedOut.map((b) => b.file).sort(),
    keypad.map((b) => b.file).sort(),
    "the Screens that opt out are exactly the ones drawing the PIN keypad",
  );
  assert.deepEqual([...new Set(optedOut.map((b) => b.file))].sort(), [
    "app/(auth)/pin.tsx",
    "app/self/link-device.tsx",
  ]);
  for (const { file, block } of optedOut) {
    assert.doesNotMatch(block, /<Field\b|<TextInput\b/, `${file}: an opted-out screen must have no text field`);
  }
  // link-device: the code step keeps its keyboard avoidance; only the PIN step opts out.
  const linkDevice = blocks.filter((b) => b.file === "app/self/link-device.tsx");
  assert.ok(linkDevice.some((b) => /<Field\b/.test(b.block) && !/avoidKeyboard=\{false\}/.test(b.block)));
});

test("<Screen avoidKeyboard={false}> renders no KeyboardAvoidingView", () => {
  const ui = read("components/ui.tsx");
  assert.match(ui, /avoidKeyboard = true/, "avoidance stays the default");
  assert.match(ui, /if \(!avoid\) \{\s*return <View style=\{\{ flex: 1 \}\}>\{children\}<\/View>;/);
  assert.match(ui, /<KeyboardFrame avoid=\{avoidKeyboard && !nested\}>/);
});
