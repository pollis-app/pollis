// The two QR sign-in codes, by which device shows them. Pure, so the node
// tests reach it; Rust (`pollis-core/src/commands/device_link.rs`) is the
// source of truth for the prefixes and does the real parsing.
//
// - v1 (#1207): shown by a SIGNED-IN device; the new device scans it.
// - v2 (#1361): shown by the NEW device; a signed-in device scans it.

export const SHOWN_BY_SIGNED_IN = "pollis-link:v1:";
export const SHOWN_BY_NEW_DEVICE = "pollis-link:v2:";

/** A code a new device signs in with (shown by a signed-in device). */
export function isSignInCode(text: string): boolean {
  return text.trim().startsWith(SHOWN_BY_SIGNED_IN);
}

/** A code a signed-in device scans to sign in the device showing it. */
export function isNewDeviceCode(text: string): boolean {
  return text.trim().startsWith(SHOWN_BY_NEW_DEVICE);
}

/** The platform name a phone gives itself on another device's screen. */
export function phoneDeviceName(os: string): string {
  return os === "ios" ? "Pollis on iPhone" : "Pollis on Android";
}
