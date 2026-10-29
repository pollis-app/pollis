// The two QR sign-in codes: v1 (#1207) shown by a signed-in device, v2 (#1361) by the new one. Rust parses for real.

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
