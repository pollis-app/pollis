// The Android intake's native side (#2100): `PollisShareModule.kt` in modules/pollis-native. iOS
// shares never reach the app (its share extension sends in place), so every call is a no-op there,
// and on a binary built before the module.

import { NativeModules, Platform } from "react-native";
import { parseManifest, type ShareDraft } from "./shareRules";

type ShareModule = {
  takePendingShare(): Promise<string | null>;
  discardShare(id: string): Promise<void>;
  discardAllShares(): Promise<void>;
  setShareTargetEnabled(enabled: boolean): Promise<void>;
};

function module(): ShareModule | null {
  if (Platform.OS !== "android") {
    return null;
  }
  return (NativeModules as { PollisShare?: ShareModule }).PollisShare ?? null;
}

/** The share another app sent, until it is sent or cancelled (`discardShare`). */
export async function takePendingShare(): Promise<ShareDraft | null> {
  const native = module();
  if (!native) {
    return null;
  }
  try {
    return parseManifest(await native.takePendingShare());
  } catch {
    return null;
  }
}

/** Delete a share's copied files (sent, or cancelled). */
export async function discardShare(id: string): Promise<void> {
  try {
    await module()?.discardShare(id);
  } catch {
    // Best effort: the intake sweeps a day-old share anyway.
  }
}

/** Delete every share's files (sign-out). */
export async function discardAllShares(): Promise<void> {
  try {
    await module()?.discardAllShares();
  } catch {
    // Best effort, as above.
  }
}

/** Offer Pollis in other apps' share sheets only while someone is signed in. */
export async function setShareTargetEnabled(enabled: boolean): Promise<void> {
  try {
    await module()?.setShareTargetEnabled(enabled);
  } catch {
    // The manifest keeps it off until the first successful enable.
  }
}
