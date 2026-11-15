/*
 * #1924: who is asked to pick Terminal or Modern after sign-in, and what "Modern" stores.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_SKIN,
  MODERN_SKIN,
  hasStoredSkin,
  isFreshSignIn,
  needsAppearanceChoice,
} from "../src/utils/firstRunAppearance.ts";

const NEW_ACCOUNT = { user: { id: "u1" }, newSecretKey: "A3-KEY", enrollmentRequired: false };
const ENROLLING = { user: { id: "u1" }, newSecretKey: null, enrollmentRequired: true };
const RETURNING = { user: { id: "u1" }, newSecretKey: null, enrollmentRequired: false };

test("Modern is stored as 'refined', which older desktop builds still read", () => {
  // Older builds' normalizeSkin maps anything but "refined" to terminal; renaming the wire value
  // would flip a user's not-yet-updated devices back to terminal.
  assert.equal(MODERN_SKIN, "refined");
  assert.equal(DEFAULT_SKIN, "terminal");
});

test("a new account and a first enrollment on this install are fresh sign-ins", () => {
  assert.equal(isFreshSignIn(NEW_ACCOUNT, []), true);
  assert.equal(isFreshSignIn(ENROLLING, ["someone-else"]), true);
});

test("a returning sign-in, or an install that already knows the account, is not fresh", () => {
  assert.equal(isFreshSignIn(RETURNING, []), false);
  // Forgot-PIN recovery enrolls again on an install that knows the account.
  assert.equal(isFreshSignIn(ENROLLING, ["u1"]), false);
  assert.equal(isFreshSignIn(NEW_ACCOUNT, ["u1"]), false);
});

test("a stored skin is any string under 'skin'", () => {
  assert.equal(hasStoredSkin(JSON.stringify({ skin: "terminal" })), true);
  assert.equal(hasStoredSkin(JSON.stringify({ skin: "refined" })), true);
  assert.equal(hasStoredSkin(JSON.stringify({ accent_hex: "#fff" })), false);
  assert.equal(hasStoredSkin("{}"), false);
  assert.equal(hasStoredSkin("not json"), false);
  assert.equal(hasStoredSkin(null), false);
});

test("only a fresh sign-in on an account with no skin is asked", () => {
  assert.equal(needsAppearanceChoice(true, "{}"), true);
  // Chosen on another device: inherit it.
  assert.equal(needsAppearanceChoice(true, JSON.stringify({ skin: "refined" })), false);
  assert.equal(needsAppearanceChoice(false, "{}"), false);
  // Preferences unreadable: don't ask rather than risk asking an existing user.
  assert.equal(needsAppearanceChoice(true, null), false);
});
