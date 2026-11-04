// What the PIN screen shows (app/(auth)/pin.tsx). Pure, so tests/pin-stage.test.ts can pin it.

export type PinStage = "checking" | "create-first" | "create-confirm" | "unlock";

/** Which stages hand the fourth digit to a slow call (Argon2id + opening the encrypted DB). */
export function pinSubmitIsSlow(stage: PinStage): boolean {
  return stage === "unlock" || stage === "create-confirm";
}

export type PinScreenView = "keypad" | "checking" | "submitting";

/**
 * The launch loader while the screen reads the unlock state (so boot → PIN shows one loader, not
 * two), the keypad until a PIN is in, then the loader again in its place, held until the screen
 * navigates away or the call fails (a wrong PIN puts the keypad back, with its error). An error
 * always shows the pad, which carries the way out (sign in again).
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
  return "keypad";
}
