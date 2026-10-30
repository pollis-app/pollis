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

test("the bridge's arms take the hook's argument names", () => {
  const bridge = read("../../pollis-core/src/bridge.rs");
  assert.match(bridge, /"get_notification_content" =>/);
  assert.match(bridge, /"set_notification_content" => \{[\s\S]*?arg\(&args, "mode"\)/);
});

test("the setting is a radio list, one row per choice, after the permission row", () => {
  const screen = read("../app/self/preferences.tsx");
  assert.match(screen, /<NotificationsSetting \/>\s*<\/Group>\s*<NotificationContentSetting \/>/);
  assert.match(screen, /<RadioRow[\s\S]*?testID=\{`radio-notification-content-\$\{opt\}`\}/);
  assert.match(screen, /accessibilityRole="radiogroup"/);
  assert.doesNotMatch(screen, /ChoiceChip/);
  assert.match(screen, /notificationContent\.description/);
  assert.match(screen, /notificationContent\.pinNote/);
});

test("a radio row is spoken as a radio and selection is not colour alone", () => {
  const row = read("../components/RadioRow.tsx");
  assert.match(row, /accessibilityRole="radio"/);
  assert.match(row, /accessibilityState=\{\{ checked: selected, selected \}\}/);
  assert.match(row, /selected \? <Icon\.check/);
  assert.match(row, /fontFamily: selected \? fonts\.bold/);
  // Controls are never bordered (design rule): selection is a fill.
  assert.doesNotMatch(row, /border(Width|Start|Left|Color|Style)/);
});

test("every locale carries the copy", () => {
  for (const lng of ["en", "ar", "es", "fr", "ru", "uk", "zh"]) {
    const cat = JSON.parse(read(`../../frontend/src/i18n/locales/${lng}/mobile.json`));
    const nc = cat.self.preferences.notificationContent;
    for (const k of ["heading", "description", "full", "fullSub", "name", "nameSub", "none", "noneSub", "pinNote"]) {
      assert.ok(typeof nc?.[k] === "string" && nc[k].length > 0, `${lng}: ${k}`);
    }
  }
});
