// Sharing into Pollis on Android (#2100): the share sheet shows Pollis, the app, and never a
// person, group or channel. These pin what Android would need to offer more: Sharing Shortcuts
// (a `<share-target>` in a shortcuts.xml), direct share via ChooserTargetService, or dynamic
// shortcuts published from code. And they pin the intake: one activity takes SEND, disabled
// until someone signs in, for text, links, images, video and files only.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const mobile = join(import.meta.dirname, "..");
const moduleManifest = readFileSync(
  join(mobile, "modules/pollis-native/android/src/main/AndroidManifestNew.xml"),
  "utf8",
);

function files(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    if (name === "node_modules" || name === "build" || name === ".gradle" || name === "jniLibs") {
      continue;
    }
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      files(p, out);
    } else if (/\.(xml|kt|java|js|json)$/.test(name)) {
      out.push(p);
    }
  }
  return out;
}

const sources = [
  ...files(join(mobile, "modules/pollis-native/android/src")),
  ...files(join(mobile, "plugins")),
  join(mobile, "app.json"),
];

/** The `<activity …>…</activity>` blocks of the module manifest, by name. */
function activities(xml: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const m of xml.matchAll(/<activity\b[^>]*\/>|<activity\b[^>]*[^/]>[\s\S]*?<\/activity>/g)) {
    const name = /android:name="([^"]+)"/.exec(m[0])?.[1];
    if (name) {
      out.set(name, m[0]);
    }
  }
  return out;
}

test("no Sharing Shortcuts, share targets, chooser targets or published shortcuts anywhere", () => {
  for (const file of sources) {
    const text = readFileSync(file, "utf8");
    const where = relative(mobile, file);
    for (const banned of [
      "<share-target",
      "chooser_target_service",
      "ChooserTargetService",
      "android.app.shortcuts",
      "ShortcutManagerCompat",
      "pushDynamicShortcut",
      "setDynamicShortcuts",
      "addDynamicShortcuts",
      "setLongLived",
      "SHORTCUT_CATEGORY",
    ]) {
      // This test names them; nothing else may.
      assert.ok(!text.includes(banned), `${where} mentions ${banned}`);
    }
  }
});

test("only ShareIntakeActivity takes SEND, and it starts disabled", () => {
  const all = activities(moduleManifest);
  const intake = all.get("com.pollisnative.share.ShareIntakeActivity");
  assert.ok(intake, "the intake activity is declared");
  assert.match(intake, /android:enabled="false"/, "off until someone signs in");
  assert.match(intake, /android:exported="true"/);
  assert.match(intake, /android:excludeFromRecents="true"/);
  assert.match(intake, /android:taskAffinity=""/);
  for (const [name, block] of all) {
    if (name !== "com.pollisnative.share.ShareIntakeActivity") {
      assert.ok(!/android\.intent\.action\.SEND/.test(block), `${name} must not take shares`);
    }
  }
  assert.ok(!/<meta-data[^>]*chooser/i.test(intake));
});

test("the intake declares only text, images, video, audio and files", () => {
  const intake = activities(moduleManifest).get("com.pollisnative.share.ShareIntakeActivity")!;
  const types = [...intake.matchAll(/android:mimeType="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(
    [...new Set(types)].sort(),
    ["application/*", "audio/*", "image/*", "text/*", "video/*"],
  );
  assert.ok(!types.includes("*/*"), "never everything");
  assert.match(intake, /android\.intent\.action\.SEND"/);
  assert.match(intake, /android\.intent\.action\.SEND_MULTIPLE"/);
});

test("the intake reads only other apps' content providers, and safe names", () => {
  const inbox = readFileSync(
    join(mobile, "modules/pollis-native/android/src/main/java/com/pollisnative/share/ShareInbox.kt"),
    "utf8",
  );
  assert.match(inbox, /uri\.scheme != ContentResolver\.SCHEME_CONTENT/, "no file:// URIs");
  assert.match(inbox, /authority\.startsWith\(context\.packageName\)/, "never Pollis's own providers");
  assert.match(inbox, /MAX_BYTES: Long = 100L \* 1024 \* 1024/);
  assert.match(inbox, /MAX_ITEMS = 10/);
});

test("sign-out turns the share target off and deletes waiting shares", () => {
  const session = readFileSync(join(mobile, "lib/session/index.ts"), "utf8");
  assert.match(session, /setShareTargetEnabled\(false\)/);
  assert.match(session, /discardAllShares\(\)/);
  const hook = readFileSync(join(mobile, "hooks/useShareIntake.ts"), "utf8");
  assert.match(hook, /setShareTargetEnabled\(true\)/);
});
