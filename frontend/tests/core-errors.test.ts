/* pollis-core's PIN and lock errors reach desktop screens in the user's language (#1724); mobile/tests/core-errors.test.ts pins the Rust strings and the mobile copy. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import i18n from "i18next";

import { isPinLockedOut, localizeCoreError, localizeCoreFailure } from "../src/i18n/coreErrors.ts";

const catalogue = (lng: string) =>
  JSON.parse(readFileSync(join(import.meta.dirname, `../src/i18n/locales/${lng}/errors.json`), "utf8"));

await i18n.init({
  lng: "fr",
  fallbackLng: "en",
  ns: ["errors"],
  resources: { en: { errors: catalogue("en") }, fr: { errors: catalogue("fr") } },
});

test("a Tauri rejection (a plain string) comes back translated", () => {
  assert.equal(localizeCoreFailure("pin incorrect; 3 attempts remaining"), "Code PIN incorrect. Il vous reste 3 tentatives.");
  assert.equal(localizeCoreError("pin incorrect; 1 attempts remaining"), "Code PIN incorrect. Il vous reste 1 tentative.");
});

test("a lockout is recognised before and after translation", () => {
  const raw = "pin locked out; use Secret Key recovery";
  assert.ok(isPinLockedOut(raw));
  assert.ok(isPinLockedOut(localizeCoreError(raw)));
});

test("anything unmapped is untouched", () => {
  assert.equal(localizeCoreFailure("device_revoked"), "device_revoked");
});
