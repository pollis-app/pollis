/* pollis-core's PIN and lock errors reach the screen in the user's language (#1724). */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import i18n from "i18next";

import { isPinLockedOut, localizeCoreError, localizeCoreFailure } from "../i18n/coreErrors.ts";

const here = import.meta.dirname;
const catalogue = (lng: string) =>
  JSON.parse(readFileSync(join(here, `../../frontend/src/i18n/locales/${lng}/errors.json`), "utf8"));

await i18n.init({
  lng: "en",
  fallbackLng: "en",
  ns: ["errors"],
  resources: { en: { errors: catalogue("en") }, ru: { errors: catalogue("ru") } },
});

test("the mobile copy is the desktop module, byte for byte", () => {
  assert.equal(
    readFileSync(join(here, "../i18n/coreErrors.ts"), "utf8"),
    readFileSync(join(here, "../../frontend/src/i18n/coreErrors.ts"), "utf8"),
  );
});

test("the Rust messages it maps still exist, worded as it expects", () => {
  const pin = readFileSync(join(here, "../../pollis-core/src/commands/pin.rs"), "utf8");
  const unlock = readFileSync(join(here, "../../pollis-core/src/commands/device_unlock.rs"), "utf8");
  assert.ok(pin.includes('"pin incorrect; {attempts_remaining} attempts remaining"'));
  assert.ok(pin.includes('"pin locked out; use Secret Key recovery"'));
  assert.ok(pin.includes('"PIN not set for user {user_id}"'));
  assert.ok(pin.includes('"no active user; call verify_otp first"'));
  assert.ok(unlock.includes('"no active user"'));
  assert.ok(unlock.includes('"unlock Pollis first"'));
});

test("wrong PIN, with a plural count, in English and Russian", async () => {
  await i18n.changeLanguage("en");
  assert.equal(localizeCoreError("pin incorrect; 9 attempts remaining"), "Wrong PIN. 9 attempts remaining.");
  assert.equal(localizeCoreError("pin incorrect; 1 attempts remaining"), "Wrong PIN. 1 attempt remaining.");
  await i18n.changeLanguage("ru");
  assert.equal(localizeCoreError("pin incorrect; 3 attempts remaining"), "Неверный PIN-код. Осталось 3 попытки.");
  assert.equal(localizeCoreError("pin incorrect; 5 attempts remaining"), "Неверный PIN-код. Осталось 5 попыток.");
  await i18n.changeLanguage("en");
});

test("the last wrong PIN and a lockout both say what to do next", () => {
  const locked = "Too many wrong PINs. Recover your account with your Secret Key.";
  assert.equal(localizeCoreError("pin incorrect; 0 attempts remaining"), locked);
  assert.equal(localizeCoreError("pin locked out; use Secret Key recovery"), locked);
  assert.ok(isPinLockedOut("pin locked out; use Secret Key recovery"));
  assert.ok(isPinLockedOut(locked));
  assert.ok(!isPinLockedOut("pin incorrect; 4 attempts remaining"));
});

test("anything else passes through untouched", () => {
  assert.equal(localizeCoreError("device_revoked"), "device_revoked");
  assert.equal(localizeCoreError("Network error: timed out"), "Network error: timed out");
  const err = new Error("boom");
  assert.equal(localizeCoreFailure(err), err);
  assert.equal(localizeCoreFailure("upgrade_required"), "upgrade_required");
  const mapped = localizeCoreFailure(new Error("unlock Pollis first"));
  assert.ok(mapped instanceof Error);
  assert.equal((mapped as Error).message, "Unlock Pollis first.");
  assert.equal(localizeCoreFailure("no active user"), "You're not signed in on this device. Sign in again.");
});
