// The two QR sign-in codes, by which device shows them; Rust
// (`pollis-core/src/commands/device_link.rs`) owns the prefixes and parsing.
// v1 (#1207) is shown by a SIGNED-IN device, v2 (#1361) by the NEW device.

export const SHOWN_BY_SIGNED_IN = "pollis-link:v1:";
export const SHOWN_BY_NEW_DEVICE = "pollis-link:v2:";

export function isSignInCode(text: string): boolean {
  return text.trim().startsWith(SHOWN_BY_SIGNED_IN);
}

export function isNewDeviceCode(text: string): boolean {
  return text.trim().startsWith(SHOWN_BY_NEW_DEVICE);
}

export function phoneDeviceName(os: string): string {
  return os === "ios" ? "Pollis on iPhone" : "Pollis on Android";
}
