/* Holding a notification tap until the app can route it (#1422). */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { createTapGate } from "../lib/push/tapGate.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function recorder() {
  const opened: string[] = [];
  const gate = createTapGate<string>((t) => {
    opened.push(t);
  });
  return { opened, gate };
}

test("a tap while ready opens at once", () => {
  const { opened, gate } = recorder();
  gate.setReady(true);
  gate.tap("conv-1");
  assert.deepEqual(opened, ["conv-1"]);
});

test("a tap before ready is held, then opened once ready", () => {
  const { opened, gate } = recorder();
  gate.tap("conv-1");
  assert.deepEqual(opened, []);
  gate.setReady(true);
  assert.deepEqual(opened, ["conv-1"]);
  gate.setReady(false);
  gate.setReady(true);
  assert.deepEqual(opened, ["conv-1"]);
});

test("the newest held tap wins", () => {
  const { opened, gate } = recorder();
  gate.tap("conv-1");
  gate.tap("conv-2");
  gate.setReady(true);
  assert.deepEqual(opened, ["conv-2"]);
});

test("a lock holds taps again", () => {
  const { opened, gate } = recorder();
  gate.setReady(true);
  gate.setReady(false);
  gate.tap("conv-1");
  assert.deepEqual(opened, []);
  gate.setReady(true);
  assert.deepEqual(opened, ["conv-1"]);
});

test("sign-out drops a held tap", () => {
  const { opened, gate } = recorder();
  gate.tap("conv-1");
  gate.clear();
  gate.setReady(true);
  assert.deepEqual(opened, []);
});

test("the push hook routes the launch tap and every live tap through the gate", () => {
  const hook = readFileSync(join(root, "hooks/usePushNotifications.ts"), "utf8");
  assert.match(hook, /takeLaunchNotificationData\(\)/);
  assert.match(hook, /onTap: \(data\) => \{\s*gate\.tap\(data\);/);
  assert.match(hook, /gate\.setReady\(ready\)/);
  const push = readFileSync(join(root, "lib/push/index.ts"), "utf8");
  assert.match(push, /clearLastNotificationResponse\(\)/);
});
