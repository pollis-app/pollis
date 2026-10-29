/*
 * Touch-down activation where an accidental press is free to undo (#1529).
 *
 * The guard is pure (lib/pressIn.ts), so its rule is pinned directly: a
 * finger runs the action once, on touch-down; a screen reader's click (no
 * touch-down) runs it once, on press; a touch that slid off never swallows a
 * later screen-reader activation. The call sites are pinned at the source
 * level: the fixed chrome the issue lists carries `pressIn`, and the controls
 * it excludes (send, destructive, confirms, rows inside a scroller) do not.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createPressInGuard, pressInAllowed, pressInSupported } from "../lib/pressIn.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

// A manual scheduler: the test decides when the "next tick" runs.
function manual() {
  const queue: (() => void)[] = [];
  return {
    schedule: (fn: () => void) => {
      queue.push(fn);
    },
    flush: () => {
      while (queue.length) {
        queue.shift()!();
      }
    },
  };
}

test("a tap runs the action once, on touch-down", () => {
  const tick = manual();
  const g = createPressInGuard(tick.schedule);
  let runs = 0;
  const action = () => runs++;
  g.onPressIn(action);
  assert.equal(runs, 1, "touch-down acts at once");
  // Pressable's order on a short tap: onPress, then onPressOut after its
  // minimum press duration.
  g.onPress(action);
  g.onPressOut();
  tick.flush();
  assert.equal(runs, 1, "the release of the same touch is swallowed");
});

test("a long hold runs it once whichever order Pressable reports", () => {
  const tick = manual();
  const g = createPressInGuard(tick.schedule);
  let runs = 0;
  const action = () => runs++;
  g.onPressIn(action);
  // A hold past the minimum press duration: onPressOut first, synchronously,
  // then onPress in the same handler, before any tick.
  g.onPressOut();
  g.onPress(action);
  tick.flush();
  assert.equal(runs, 1);
});

test("a screen reader's click (no touch-down) runs it on press", () => {
  const tick = manual();
  const g = createPressInGuard(tick.schedule);
  let runs = 0;
  const action = () => runs++;
  g.onPress(action);
  assert.equal(runs, 1);
  g.onPress(action);
  assert.equal(runs, 2, "every activation acts");
});

test("a touch that slid off does not swallow a later screen-reader click", () => {
  const tick = manual();
  const g = createPressInGuard(tick.schedule);
  let runs = 0;
  const action = () => runs++;
  g.onPressIn(action);
  // Slid off: onPressOut, no onPress.
  g.onPressOut();
  tick.flush();
  g.onPress(action);
  assert.equal(runs, 2);
});

test("taps in a row each act once", () => {
  const tick = manual();
  const g = createPressInGuard(tick.schedule);
  let runs = 0;
  const action = () => runs++;
  for (let i = 0; i < 3; i++) {
    g.onPressIn(action);
    g.onPress(action);
    g.onPressOut();
    tick.flush();
  }
  assert.equal(runs, 3);
});

test("pressIn is refused together with a long press", () => {
  assert.equal(pressInAllowed(true, false), true);
  assert.equal(pressInAllowed(true, true), false);
  assert.equal(pressInAllowed(false, false), false);
  assert.equal(pressInAllowed(undefined, false), false);
  const ui = read("components/ui.tsx");
  // The types refuse the pair, and ListRow re-checks it at runtime.
  assert.match(ui, /\| \{ pressIn: true; onLongPress\?: undefined \}/);
  assert.match(ui, /usePressIn\(disabled \? undefined : onPress, pressInAllowed\(pressIn, !!onLongPress\)\)/);
});

test("Android keeps every control inside a sheet on release", () => {
  // Dismissing a Modal on touch-down strands that touch on Android, and the
  // next tap on the screen underneath was lost (the tour's first Back).
  assert.equal(pressInSupported("android", true), false);
  assert.equal(pressInSupported("android", false), true);
  assert.equal(pressInSupported("ios", true), true);
  assert.equal(pressInSupported("ios", false), true);
  const overlay = read("components/chat/SheetOverlay.tsx");
  // The provider wraps the whole Modal body: the backdrop, Close and rows.
  const start = overlay.indexOf("<InSheetContext.Provider value={true}>");
  const end = overlay.indexOf("</InSheetContext.Provider>");
  assert.ok(start > 0 && end > start);
  const body = overlay.slice(start, end);
  for (const part of ["accessible={false}", "testID={closeTestID}", "{children}"]) {
    assert.ok(body.includes(part), `${part} must sit inside the sheet context`);
  }
});

test("the shared primitives take pressIn and keep onPress for screen readers", () => {
  const ui = read("components/ui.tsx");
  for (const call of [
    "usePressIn(disabled ? undefined : onPress, pressIn)",
    "usePressIn(off ? undefined : onPress, pressIn)",
    "usePressIn(onTitlePress, true)",
  ]) {
    assert.ok(ui.includes(call), `ui.tsx must call ${call}`);
  }
  // The hook always hands Pressable an onPress (TalkBack activates through
  // it), and adds onPressIn only when opted in.
  const hook = ui.slice(ui.indexOf("export function usePressIn"), ui.indexOf("export function PressInPressable"));
  assert.match(hook, /if \(!enabled \|\| !onPress \|\| !pressInSupported\(Platform\.OS, inSheet\)\) \{\s*return \{ onPress \};/);
  assert.match(hook, /onPress: \(\) => g\.onPress\(run\)/);
  // Navigation never plays a haptic (mobile/CLAUDE.md).
  assert.doesNotMatch(hook, /haptic/);
  // The Header's back acts on touch-down on every pushed screen.
  const header = ui.slice(ui.indexOf("export function Header"), ui.indexOf("/* ── Section title"));
  assert.match(header, /testID=\{backTestID\}[\s\S]*?pressIn\n/);
});

// The fixed chrome #1529 converts: file → the testIDs (or markers) that must
// carry `pressIn` on the line after them or within the same element.
const CONVERTED: Record<string, string[]> = {
  "components/TabBar.tsx": ["<PressInPressable\n            key={tab.name}\n            pressIn"],
  "components/chat/SheetOverlay.tsx": [
    "<PressInPressable\n              pressIn\n              onPress={onClose}\n              accessible={false}",
    "<PressInPressable\n                  pressIn\n                  onPress={onClose}\n                  testID={closeTestID}",
  ],
  "app/chat/[id].tsx": ['testID="btn-pins"\n              pressIn', 'testID="btn-members"\n              pressIn', 'testID="btn-chat-menu"\n              pressIn'],
  "components/chat/Composer.tsx": ['pressIn\n          testID="btn-attach"', 'pressIn\n                testID="btn-composer-emoji"'],
  "components/chat/ReplyBar.tsx": ["pressIn\n        onPress={onPressQuote}"],
  "components/groups/GroupPanel.tsx": ['pressIn\n          testID="btn-group-menu"', 'testID="row-group-invite"\n          pressIn', 'pressIn\n          testID="btn-search-group"'],
  "app/(tabs)/direct.tsx": ['testID="btn-new-dm"\n            pressIn'],
  "app/vault.tsx": ['testID="chip-vault-view-chat"\n          pressIn', 'testID="chip-vault-view-media"\n          pressIn'],
  "app/media.tsx": ['testID="btn-media-open-settings"\n                  pressIn', 'testID="btn-media-prev"\n                pressIn', 'testID="btn-media-next"\n                pressIn'],
  "components/chat/ChannelMenuSheet.tsx": ['testID="btn-menu-info"\n          pressIn', 'testID="btn-menu-group-settings"\n            pressIn'],
  "components/groups/GroupMenuSheet.tsx": [
    'testID="btn-menu-members"\n          pressIn',
    'testID="btn-menu-invite"\n          pressIn',
    'testID="btn-menu-group-settings"\n          pressIn',
    'testID="btn-menu-requests"\n            pressIn',
  ],
  "components/groups/AddGroupSheet.tsx": ['testID="btn-create-group"\n          pressIn', 'testID="btn-join-group"\n          pressIn'],
  "components/media/AudioPage.tsx": [
    'testID="btn-media-audio-back"\n            pressIn',
    'pressIn\n            testID="btn-media-audio-play"',
    'testID="btn-media-audio-forward"\n            pressIn',
  ],
};

test("the fixed chrome #1529 lists acts on touch-down", () => {
  for (const [rel, needles] of Object.entries(CONVERTED)) {
    const src = read(rel);
    for (const needle of needles) {
      assert.ok(src.includes(needle), `${rel} must carry pressIn at: ${needle.split("\n")[0]}`);
    }
  }
});

// The opening tag around a testID: from its `<` to the `>` that closes it
// at brace depth 0 (so `icon={<Icon />}` and arrow functions stay inside).
function openingTagAround(src: string, testID: string): string {
  const at = src.indexOf(`testID="${testID}"`);
  assert.ok(at >= 0, `testID ${testID} not found`);
  const start = src.lastIndexOf("<", at);
  let depth = 0;
  for (let i = start + 1; i < src.length; i++) {
    const c = src[i];
    if (c === "{") {
      depth++;
    } else if (c === "}") {
      depth--;
    } else if (c === ">" && depth === 0) {
      return src.slice(start, i + 1);
    }
  }
  return src.slice(start);
}

test("sending, destructive and confirming controls stay on release", () => {
  const exclusions: [string, string][] = [
    ["components/chat/Composer.tsx", "btn-send"],
    ["components/chat/ReplyBar.tsx", "btn-reply-dismiss"],
    ["components/groups/GroupMenuSheet.tsx", "btn-leave-group"],
    ["app/media.tsx", "btn-media-save"],
    ["app/media.tsx", "btn-media-share"],
    ["app/vault.tsx", "btn-vault-retry"],
  ];
  for (const [rel, id] of exclusions) {
    const src = read(rel);
    assert.doesNotMatch(openingTagAround(src, id), /\bpressIn\b/, `${rel} ${id} must stay on release`);
  }
  // The audio seek bar reads where the finger lifts.
  const audio = read("components/media/AudioPage.tsx");
  const seek = audio.slice(audio.indexOf("onLayout={(e) => setTrackWidth"), audio.indexOf("text-media-audio-clock"));
  assert.doesNotMatch(seek, /\bpressIn\b/);
  // Toggles and confirms never take it.
  const ui = read("components/ui.tsx");
  for (const fn of ["export function Toggle(", "export function Checkbox(", "export function CheckRow("]) {
    const start = ui.indexOf(fn);
    const body = ui.slice(start, ui.indexOf("\n}\n", start));
    assert.doesNotMatch(body, /pressIn/i, `${fn} must stay on release`);
  }
});

test("rows inside a scroller and message surfaces stay on release", () => {
  // Touch-down lands before a scroll takes the touch, so a row in a
  // ScrollView / FlatList would open on every scroll that starts on it; the
  // message sheet's actions notify others; long-press surfaces must not act
  // on touch-down at all.
  for (const rel of [
    "app/(tabs)/groups.tsx",
    "app/(tabs)/self.tsx",
    "app/(tabs)/search.tsx",
    "app/conversation/pins.tsx",
    "app/conversation/info.tsx",
    "app/self/saved.tsx",
    "app/self/security.tsx",
    "components/chat/MessageRow.tsx",
    "components/chat/MessageActionsSheet.tsx",
    "components/chat/SwipeToReply.tsx",
  ]) {
    const src = read(rel);
    assert.doesNotMatch(src, /\bpressIn\b/, `${rel} must not act on touch-down`);
  }
  // Direct's conversation rows scroll; only its header button converts.
  const direct = read("app/(tabs)/direct.tsx");
  assert.equal(direct.match(/\bpressIn\b/g)?.length, 1);
  // GroupPanel: only the fixed header, never the scrolling channel rows.
  const panel = read("components/groups/GroupPanel.tsx");
  const scroller = panel.slice(panel.indexOf("<ScrollView"), panel.indexOf("</ScrollView>"));
  assert.doesNotMatch(scroller, /\bpressIn\b/);
});
