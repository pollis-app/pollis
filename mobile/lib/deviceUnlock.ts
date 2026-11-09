// Biometric unlock (#1685): Face ID / Touch ID / a Class 3 fingerprint stands in for TYPING the
// PIN at unlock, and for nothing else. Core makes the device key and wraps the same keys the PIN
// does (pollis-core/src/commands/device_unlock.rs); this file keeps that key in the platform's
// biometric-bound store:
//
// - iOS: a keychain item with `.biometryCurrentSet` (expo-secure-store hard-codes it for
//   `requireAuthentication`), `WHEN_PASSCODE_SET_THIS_DEVICE_ONLY`, in the app-only access group
//   named by the embedded config. Never the default group: that is the shared one the
//   Notification Service Extension reads.
// - Android: an AndroidKeyStore key with per-use user authentication, used through a
//   BiometricPrompt `CryptoObject` (BIOMETRIC_STRONG only), invalidated on enrolment changes.
//
// A cancelled or failed read never costs a PIN attempt; it just shows the keypad.
import { Platform } from "react-native";
import * as SecureStore from "expo-secure-store";
import i18n from "../i18n";
import { invoke } from "./native";
import { embeddedExtra } from "./buildInfo";
import {
  DEVICE_UNLOCK_SERVICE,
  classifyRead,
  deviceKeyName,
  deviceUnlockSupported as supportedBy,
  iosAccessGroup,
  offeredKeyName,
  type ReadOutcome,
} from "./deviceUnlockRules";

export type { ReadOutcome } from "./deviceUnlockRules";

function canUseBiometrics(): boolean {
  try {
    return SecureStore.canUseBiometricAuthentication();
  } catch {
    return false;
  }
}

/** Whether this device can offer biometric unlock right now. */
export function deviceUnlockSupported(): boolean {
  return supportedBy(Platform.OS, embeddedExtra(), canUseBiometrics());
}

/** "Face ID or Touch ID" / "fingerprint or face unlock", for the copy. */
export function biometricMethod(): string {
  return Platform.OS === "ios"
    ? i18n.t("mobile:biometric.methodIos")
    : i18n.t("mobile:biometric.methodAndroid");
}

/** Where the key lives. Null when it must not be stored (no app-only group on iOS). */
function keyOptions(withPrompt: boolean): SecureStore.SecureStoreOptions | null {
  const options: SecureStore.SecureStoreOptions = {
    keychainService: DEVICE_UNLOCK_SERVICE,
    requireAuthentication: true,
    keychainAccessible: SecureStore.WHEN_PASSCODE_SET_THIS_DEVICE_ONLY,
  };
  if (withPrompt) {
    options.authenticationPrompt = i18n.t("mobile:biometric.prompt");
  }
  if (Platform.OS === "ios") {
    const group = iosAccessGroup(embeddedExtra());
    if (!group) {
      return null;
    }
    options.accessGroup = group;
  }
  return options;
}

/** Read the device key; shows the system prompt. */
export async function readDeviceKey(userId: string): Promise<ReadOutcome> {
  const name = deviceKeyName(userId);
  const options = keyOptions(true);
  if (!name || !options) {
    return { kind: "cancelled" };
  }
  try {
    return classifyRead(await SecureStore.getItemAsync(name, options), false);
  } catch {
    return classifyRead(null, true);
  }
}

async function deleteDeviceKey(userId: string): Promise<void> {
  const name = deviceKeyName(userId);
  const options = keyOptions(false);
  if (!name || !options) {
    return;
  }
  try {
    await SecureStore.deleteItemAsync(name, options);
  } catch (e) {
    console.warn("[deviceUnlock] deleting the device key failed:", e);
  }
}

/**
 * Turn biometric unlock on for the unlocked user. `pin` is what they typed for it; without one,
 * core accepts it only right after a PIN unlock (the one-time offer, and re-arming).
 * `readBack` reads the key back before confirming (iOS: one Face ID prompt that also proves it
 * works; skipped on Android, where storing already took the prompt). Throws on any failure and
 * leaves it off.
 */
export async function enableDeviceUnlock(
  userId: string,
  { pin, readBack }: { pin?: string; readBack: boolean },
): Promise<void> {
  const name = deviceKeyName(userId);
  const options = keyOptions(true);
  if (!name || !options || !canUseBiometrics()) {
    throw new Error(i18n.t("mobile:biometric.failed"));
  }
  const key = await invoke<string>("device_unlock_begin", { pin: pin ?? null });
  try {
    await SecureStore.setItemAsync(name, key, options);
    if (readBack && Platform.OS === "ios") {
      const back = await SecureStore.getItemAsync(name, options);
      if (back !== key) {
        throw new Error(i18n.t("mobile:biometric.failed"));
      }
    }
    await invoke("device_unlock_confirm", { deviceKey: key });
  } catch (e) {
    // Begun but not confirmed is already off in core; clear what was written.
    await disableDeviceUnlock(userId).catch(() => {});
    throw e;
  }
}

/** Turn it off: core's slots and the stored key. Needs no PIN. */
export async function disableDeviceUnlock(userId: string): Promise<void> {
  try {
    await invoke("disable_device_unlock");
  } finally {
    await deleteDeviceKey(userId);
  }
}

/** Unlock with a key `readDeviceKey` returned. */
export async function unlockWithDeviceKey(userId: string, deviceKey: string): Promise<void> {
  await invoke("unlock_with_device_key", { userId, deviceKey });
}

/** Drop the stored key when the account's data goes (sign out and delete, account deletion). */
export async function forgetDeviceKey(userId: string): Promise<void> {
  await deleteDeviceKey(userId);
  const offered = offeredKeyName(userId);
  if (offered) {
    await SecureStore.deleteItemAsync(offered).catch(() => {});
  }
}

// Set when the PIN screen found biometric unlock on but its key gone (re-enrolled); the next PIN
// unlock re-arms it. Memory only: a cold start finds the same state again.
let rearmUser: string | null = null;

export function markNeedsRearm(userId: string): void {
  rearmUser = userId;
}

export function takeNeedsRearm(userId: string): boolean {
  const due = rearmUser === userId;
  rearmUser = null;
  return due;
}

/** Whether the one-time offer was already shown to this user on this device. */
export async function wasOffered(userId: string): Promise<boolean> {
  const name = offeredKeyName(userId);
  if (!name) {
    return true;
  }
  try {
    return (await SecureStore.getItemAsync(name)) === "1";
  } catch {
    return true;
  }
}

export async function markOffered(userId: string): Promise<void> {
  const name = offeredKeyName(userId);
  if (!name) {
    return;
  }
  try {
    await SecureStore.setItemAsync(name, "1");
  } catch (e) {
    console.warn("[deviceUnlock] recording the offer failed:", e);
  }
}
