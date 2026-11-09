// Biometric unlock (#1685): the rules, import-free so tests/device-unlock.test.ts can pin them.
// The store and the bridge calls are in lib/deviceUnlock.ts.

/** The SecureStore service the device key lives under, apart from every other item. */
export const DEVICE_UNLOCK_SERVICE = "pollis.deviceUnlock";

/**
 * The iOS keychain group the Notification Service Extension shares. The device key must NEVER
 * be written there: the extension runs with no user present.
 */
export const SHARED_KEYCHAIN_SUFFIX = ".com.pollis.shared";

/** SecureStore keys allow only `[A-Za-z0-9._-]`; a user id that does not fit gets no key. */
export function deviceKeyName(userId: string): string | null {
  if (!/^[A-Za-z0-9._-]+$/.test(userId)) {
    return null;
  }
  return `duk.${userId}`;
}

/** The plain (no biometrics) item recording that the one-time offer was shown. */
export function offeredKeyName(userId: string): string | null {
  const name = deviceKeyName(userId);
  return name ? `${name}.offered` : null;
}

/**
 * The app-only keychain access group, from the binary's EMBEDDED config (`extra.deviceUnlock`,
 * set by plugins/withDeviceUnlock.js next to the entitlement). Anything that is not
 * `<TEAMID>.<suffix>`, or that names the shared group, answers null: then the feature is off,
 * never written to the default (shared) group instead.
 */
export function iosAccessGroup(embeddedExtra: unknown): string | null {
  if (typeof embeddedExtra !== "object" || embeddedExtra === null) {
    return null;
  }
  const block = (embeddedExtra as { deviceUnlock?: unknown }).deviceUnlock;
  if (typeof block !== "object" || block === null) {
    return null;
  }
  const group = (block as { iosAccessGroup?: unknown }).iosAccessGroup;
  if (typeof group !== "string" || !/^[A-Z0-9]{10}\.[A-Za-z0-9.-]+$/.test(group)) {
    return null;
  }
  if (group.endsWith(SHARED_KEYCHAIN_SUFFIX)) {
    return null;
  }
  return group;
}

/**
 * Whether this device may offer biometric unlock. iOS needs the app-only group in the binary;
 * both need the platform to say a strong biometric is enrolled (`canUseBiometricAuthentication`,
 * BIOMETRIC_STRONG on Android, which also keeps it off where expo/expo#49330 breaks detection).
 */
export function deviceUnlockSupported(
  platform: string,
  embeddedExtra: unknown,
  canUseBiometrics: boolean,
): boolean {
  if (!canUseBiometrics) {
    return false;
  }
  if (platform === "ios") {
    return iosAccessGroup(embeddedExtra) !== null;
  }
  return platform === "android";
}

export type ReadOutcome =
  | { kind: "key"; key: string }
  // The key is gone: biometrics were re-enrolled (iOS `.biometryCurrentSet`, Android
  // KeyPermanentlyInvalidated) or the item was removed. The PIN re-arms it.
  | { kind: "invalidated" }
  // Cancelled, failed, locked out, or the prompt could not show: the keypad, nothing else.
  | { kind: "cancelled" };

/** What a read of the device key means. A thrown read is never treated as "gone". */
export function classifyRead(value: string | null | undefined, threw: boolean): ReadOutcome {
  if (threw) {
    return { kind: "cancelled" };
  }
  if (typeof value === "string" && value.length > 0) {
    return { kind: "key", key: value };
  }
  return { kind: "invalidated" };
}

/** The one-time offer after a PIN unlock: only where it can work, only if off, only once. */
export function shouldOffer(supported: boolean, enabled: boolean, offered: boolean): boolean {
  return supported && !enabled && !offered;
}

/** The Security switch shows where it can be turned on, and wherever it is on (to turn it off). */
export function showSwitch(supported: boolean, enabled: boolean): boolean {
  return supported || enabled;
}
