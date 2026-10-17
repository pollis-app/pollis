// #1361: the QR sign-in goes both ways, and each screen must take only its own
// direction's code — the scanner a code the NEW device shows (v2), the new
// device a code a signed-in device shows (v1). The prefixes are Rust's
// (`pollis-core/src/commands/device_link.rs`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { isNewDeviceCode, isSignInCode, phoneDeviceName, SHOWN_BY_NEW_DEVICE, SHOWN_BY_SIGNED_IN } from "../lib/linkCodes.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

test("the prefixes are the ones Rust writes", () => {
  const rust = read("../../pollis-core/src/commands/device_link.rs");
  assert.match(rust, new RegExp(`LINK_PAYLOAD_PREFIX: &str = "${SHOWN_BY_SIGNED_IN}"`));
  assert.match(rust, new RegExp(`OFFER_PAYLOAD_PREFIX: &str = "${SHOWN_BY_NEW_DEVICE}"`));
});

test("each direction recognises only its own code", () => {
  const v1 = "pollis-link:v1:01HLINK:BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU";
  const v2 = "  pollis-link:v2:01HOFFER:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc ";
  assert.ok(isSignInCode(v1) && !isNewDeviceCode(v1));
  assert.ok(isNewDeviceCode(v2) && !isSignInCode(v2));
  assert.ok(!isSignInCode("https://example.com") && !isNewDeviceCode("https://example.com"));
});

test("the scanner is a scanner now, and the new phone can show a code", () => {
  const scanner = read("../app/self/link-device.tsx");
  assert.match(scanner, /CameraView/);
  assert.match(scanner, /acceptDeviceOffer\(/);
  // The QR this screen used to show is gone.
  assert.doesNotMatch(scanner, /create_device_link|useCreateDeviceLink|<QrCode/);
  const signIn = read("../app/(auth)/link.tsx");
  assert.match(signIn, /<OfferCodeView/);
  assert.equal(phoneDeviceName("ios"), "Pollis on iPhone");
  assert.equal(phoneDeviceName("android"), "Pollis on Android");
});

// Neither sign-in code may ever open the app from outside it (#1361, design doc
// § v2 "Never a link"). Read with the system camera, a code is inert text; only
// the in-app scanner, behind the PIN, can act on one. Registering a handler for
// it would turn "scan this" into a one-tap path for QR-login phishing.
test("a sign-in code is never a link the OS can hand to the app", () => {
  for (const prefix of [SHOWN_BY_SIGNED_IN, SHOWN_BY_NEW_DEVICE]) {
    assert.ok(!/^(https?|pollis):/.test(prefix), `${prefix} must not be a URL the app is registered for`);
  }
  const appJson = read("../app.json");
  assert.ok(!appJson.includes("pollis-link"), "app.json must not register the pollis-link scheme");
  const schemes = [JSON.parse(appJson).expo.scheme].flat();
  assert.deepEqual(schemes, ["pollis"]);
  const plugins = readdirSync(new URL("../plugins/", import.meta.url)).map((p) => `../plugins/${p}`);
  for (const f of ["../app.config.js", ...plugins]) {
    assert.ok(!read(f).includes("pollis-link"), `${f} must not register the pollis-link scheme`);
  }
});
