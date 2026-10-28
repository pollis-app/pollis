/*
 * The empty composer is ONE line tall (chat, thread and Vault). A multiline
 * TextInput sizes to its placeholder, so a hint too long for the width wrapped
 * and made the empty field two lines tall — and on iOS the composer then
 * recorded that as its "one-line" height. The hint is drawn by the Composer as
 * a single-line, ellipsized overlay instead, and the native input gets none.
 * The rendered height is proved on device (Maestro `vault.yaml`); these pin
 * the source shape that makes it hold, plus the copy rules for the Vault hint.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
const composer = read("../components/chat/Composer.tsx");

// The props of the <Field> that renders `input-composer`, up to its
// `trailing` (the emoji button, whose own props are not the input's).
const fieldElement = (() => {
  const start = composer.indexOf("<Field");
  assert.ok(start >= 0, "Composer renders a <Field>");
  const end = composer.indexOf("trailing={", start);
  assert.ok(end > start, "the composer <Field> has a trailing slot");
  return composer.slice(start, end);
})();

test("the native composer input is given no placeholder", () => {
  assert.match(fieldElement, /testID="input-composer"/);
  assert.doesNotMatch(fieldElement, /\bplaceholder=/);
});

test("screen readers still hear the hint, on the input", () => {
  assert.match(fieldElement, /accessibilityHint=\{hint\}/);
});

test("the hint overlay is one line, ellipsized at the end, and lets touches through", () => {
  const at = composer.indexOf('testID="composer-placeholder"');
  assert.ok(at >= 0, "the overlay has its testID");
  const overlay = composer.slice(composer.lastIndexOf("<View", at), composer.indexOf("</Text>", at));
  assert.match(overlay, /numberOfLines=\{1\}/);
  assert.match(overlay, /ellipsizeMode="tail"/);
  assert.match(overlay, /pointerEvents="none"/);
  assert.match(overlay, /importantForAccessibility="no-hide-descendants"/);
  // start/end edges so it mirrors in RTL, never left/right.
  assert.match(overlay, /\bstart:/);
  assert.match(overlay, /\bend:/);
  assert.doesNotMatch(overlay, /\b(left|right):/);
  // The native placeholder's colour.
  assert.match(overlay, /color: semantic\.muted/);
});

test("the overlay shows only while the draft is empty", () => {
  const at = composer.indexOf('testID="composer-placeholder"');
  const before = composer.slice(0, at);
  assert.ok(before.lastIndexOf('draft === "" ? (') > before.lastIndexOf("/>"));
});

test("the Vault hint is plain in every locale: no dash, no ellipsis, nothing trailing", () => {
  const dir = new URL("../../frontend/src/i18n/locales/", import.meta.url);
  const locales = readdirSync(dir);
  assert.ok(locales.includes("en"));
  for (const lng of locales) {
    const vault = JSON.parse(readFileSync(new URL(`${lng}/vault.json`, dir), "utf8"));
    const hint: string = vault.placeholder;
    assert.ok(hint && hint.length > 0, `${lng}: vault:placeholder`);
    assert.doesNotMatch(hint, /[—–]|…|\.\.\./, `${lng}: ${hint}`);
    assert.equal(hint, hint.trim(), `${lng}: no surrounding space`);
    assert.doesNotMatch(hint, /[.:;!?。]$/, `${lng}: no trailing punctuation`);
  }
  const en = JSON.parse(readFileSync(new URL("en/vault.json", dir), "utf8"));
  assert.equal(en.placeholder, "Secure notes, links, or files");
});
