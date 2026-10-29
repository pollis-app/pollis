/* The empty composer is ONE line tall: the hint is a single-line overlay and the native input gets no placeholder. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");
const composer = read("../components/chat/Composer.tsx");

// The `input-composer` Field's props, up to its `trailing`.
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
  assert.match(overlay, /\bstart:/);
  assert.match(overlay, /\bend:/);
  assert.doesNotMatch(overlay, /\b(left|right):/);
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
