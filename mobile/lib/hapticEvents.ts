// Which tactile feedback each physical interaction gets (#1286). Pure, so
// tests/haptics.test.ts can pin it without a device; lib/haptics.ts plays it.
//
// iOS uses the UIKit feedback generators (impact / selection). Android uses
// View.performHapticFeedback constants, not the vibrator: they follow the
// phone's own "touch feedback" setting and feel like the rest of the system.
// performHapticFeedback ignores a constant the phone's API level predates
// (Confirm / Reject are API 30), so those are silent on older phones rather
// than wrong.

export type HapticEvent =
  // Swipe to reply crossed the threshold: letting go now replies.
  | "swipeArmed"
  // Dragged back under the threshold: letting go now cancels.
  | "swipeDisarmed"
  // A double tap started a reply.
  | "doubleTapReply"
  // A long press opened the message sheet.
  | "longPress"
  // A reaction was added or removed.
  | "reactionAdd"
  | "reactionRemove"
  // A message left the composer.
  | "send"
  // A switch flipped, or a mark toggled (saved, verified off, unblock, role).
  | "toggle"
  // The first tap of a two-tap destructive confirm: the next tap commits.
  | "confirmArm"
  // A constructive action completed (verified, reported, approved, saved…).
  | "success"
  // A destructive action completed (blocked, deleted, removed, left…).
  | "destructive"
  // An action the user took failed (wrong PIN or code, a send that failed…).
  | "error";

export type IosHaptic =
  | { kind: "impact"; style: "light" | "medium" | "heavy" }
  | { kind: "notification"; type: "success" | "warning" | "error" }
  | { kind: "selection" };

export type AndroidHaptic =
  | "context-click"
  | "clock-tick"
  | "long-press"
  | "keyboard-tap"
  | "confirm"
  | "reject";

export const HAPTICS: Record<
  HapticEvent,
  { ios: IosHaptic; android: AndroidHaptic }
> = {
  swipeArmed: { ios: { kind: "impact", style: "light" }, android: "context-click" },
  swipeDisarmed: { ios: { kind: "selection" }, android: "clock-tick" },
  doubleTapReply: { ios: { kind: "impact", style: "light" }, android: "context-click" },
  longPress: { ios: { kind: "impact", style: "medium" }, android: "long-press" },
  reactionAdd: { ios: { kind: "selection" }, android: "context-click" },
  reactionRemove: { ios: { kind: "selection" }, android: "clock-tick" },
  send: { ios: { kind: "impact", style: "light" }, android: "keyboard-tap" },
  toggle: { ios: { kind: "selection" }, android: "clock-tick" },
  confirmArm: { ios: { kind: "notification", type: "warning" }, android: "context-click" },
  success: { ios: { kind: "notification", type: "success" }, android: "confirm" },
  destructive: { ios: { kind: "impact", style: "heavy" }, android: "long-press" },
  error: { ios: { kind: "notification", type: "error" }, android: "reject" },
};

/**
 * A mutation's haptics, set as `meta: { haptic }` on its useMutation and
 * played by the MutationCache in lib/queryClient.ts — one chokepoint, so a
 * hook opts in with one line and no screen calls haptic() for a result.
 * `mutate` plays at once (an optimistic action: send, react), `success` when
 * it lands; a meta-tagged mutation that fails plays "error" unless
 * `error: false`. Either may depend on the variables.
 */
export type MutationHaptic<V = unknown> = {
  mutate?: HapticEvent | ((vars: V) => HapticEvent | null);
  success?: HapticEvent | ((vars: V) => HapticEvent | null);
  error?: false;
};

export function resolveHaptic<V>(
  spec: MutationHaptic<V>["success"],
  vars: V,
): HapticEvent | null {
  if (spec === undefined) {
    return null;
  }
  return typeof spec === "function" ? spec(vars) : spec;
}

/**
 * The haptic for a change in the swipe's armed state, or none when it did
 * not change. Called from the gesture worklet on every update, so the
 * common case (no change) costs a comparison and no bridge hop.
 */
export function armHaptic(wasArmed: boolean, isArmed: boolean): HapticEvent | null {
  "worklet";
  if (wasArmed === isArmed) {
    return null;
  }
  return isArmed ? "swipeArmed" : "swipeDisarmed";
}
