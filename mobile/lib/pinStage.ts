// What the PIN screen shows (app/(auth)/pin.tsx). Pure, so tests/pin-stage.test.ts can pin it.

// "biometric": biometric unlock is on (#1685) and the system prompt is up, or its key is being
// handed to core. A cancel, a failure or a missing key moves to "unlock" (the keypad).
export type PinStage = "checking" | "biometric" | "create-first" | "create-confirm" | "unlock";

/** Which stages hand the fourth digit to a slow call (Argon2id + opening the encrypted DB). */
export function pinSubmitIsSlow(stage: PinStage): boolean {
  return stage === "unlock" || stage === "create-confirm";
}

export type PinScreenView = "keypad" | "checking" | "biometric" | "submitting";

/**
 * The launch loader while the screen reads the unlock state (so boot → PIN shows one loader, not
 * two), the keypad until a PIN is in, then the loader again in its place, held until the screen
 * navigates away or the call fails (a wrong PIN puts the keypad back, with its error). An error
 * always shows the pad, which carries the way out (sign in again). While biometrics are tried
 * the loader stays up under the system prompt; no keypad flashes first.
 */
export function pinScreenView(
  stage: PinStage,
  submitting: boolean,
  hasError: boolean,
): PinScreenView {
  if (submitting && pinSubmitIsSlow(stage)) {
    return "submitting";
  }
  if (stage === "checking" && !hasError) {
    return "checking";
  }
  if (stage === "biometric") {
    return "biometric";
  }
  return "keypad";
}

/** Where a successful PIN unlock goes next (#1685): re-arm, the one-time offer, or straight in. */
export type AfterPinUnlock = "rearm" | "offer" | "continue";

export function afterPinUnlock({
  needsRearm,
  supported,
  enabled,
  offered,
}: {
  needsRearm: boolean;
  supported: boolean;
  enabled: boolean;
  offered: boolean;
}): AfterPinUnlock {
  if (!supported) {
    return "continue";
  }
  if (needsRearm && enabled) {
    return "rearm";
  }
  if (!enabled && !offered) {
    return "offer";
  }
  return "continue";
}
