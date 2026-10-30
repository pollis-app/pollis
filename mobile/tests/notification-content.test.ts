// #1599: the notification-content setting, stored by the core where the push extension reads it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  NOTIFICATION_CONTENT_OPTIONS,
  DEFAULT_NOTIFICATION_CONTENT,
  coerceNotificationContent,
} from "../lib/notificationContent.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

test("the three choices, default first", () => {
  assert.deepEqual([...NOTIFICATION_CONTENT_OPTIONS], ["full", "name", "none"]);
  assert.equal(DEFAULT_NOTIFICATION_CONTENT, "full");
});

test("unset reads as full; anything unknown fails closed to none", () => {
  assert.equal(coerceNotificationContent(null), "full");
  assert.equal(coerceNotificationContent(undefined), "full");
  assert.equal(coerceNotificationContent("full"), "full");
  assert.equal(coerceNotificationContent("name"), "name");
  assert.equal(coerceNotificationContent("none"), "none");
  assert.equal(coerceNotificationContent("everything"), "none");
  assert.equal(coerceNotificationContent(42), "none");
});

test("the hook speaks the bridge's command and argument names", () => {
  const hook = read("../hooks/queries/useNotificationContent.ts");
  assert.match(hook, /invoke<string>\("get_notification_content"\)/);
  assert.match(hook, /invoke\("set_notification_content", \{ mode: value \}\)/);
});

// The arms are pollis-core's (feature/1599-core-headless-fetch); skipped, visibly, until that lands.
const bridge = read("../../pollis-core/src/bridge.rs");
const armsLanded = bridge.includes('"set_notification_content"');
test(
  "the bridge's arms take the hook's argument names",
  { skip: armsLanded ? false : "core arms not merged yet (#1599 part A)" },
  () => {
    assert.match(bridge, /"get_notification_content" =>/);
    assert.match(bridge, /"set_notification_content" => \{[\s\S]*?arg\(&args, "mode"\)/);
  },
);

test("the picker sits in the notifications group, one chip per choice", () => {
  const screen = read("../app/self/preferences.tsx");
  assert.match(screen, /<NotificationsSetting \/>\s*<NotificationContentSetting \/>/);
  assert.match(screen, /testID=\{`chip-notification-content-\$\{opt\}`\}/);
  assert.match(screen, /accessibilityRole="radiogroup"/);
  assert.match(screen, /notificationContent\.pinNote/);
});

test("every locale carries the copy", () => {
  for (const lng of ["en", "ar", "es", "fr", "ru", "uk", "zh"]) {
    const cat = JSON.parse(read(`../../frontend/src/i18n/locales/${lng}/mobile.json`));
    const nc = cat.self.preferences.notificationContent;
    for (const k of ["heading", "description", "full", "name", "none", "pinNote"]) {
      assert.ok(typeof nc?.[k] === "string" && nc[k].length > 0, `${lng}: ${k}`);
    }
  }
});
