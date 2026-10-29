// Which tactile feedback each interaction gets (#1286); pure. Android uses performHapticFeedback, never the vibrator.

export type HapticEvent =
  | "swipeArmed"
  | "swipeDisarmed"
  | "doubleTapReply"
  | "longPress"
  | "reactionAdd"
  | "reactionRemove"
  | "send"
  | "keyTap"
  | "toggle"
  // The first tap of a two-tap destructive confirm.
  | "confirmArm"
  | "success"
  | "destructive"
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
  keyTap: { ios: { kind: "impact", style: "light" }, android: "keyboard-tap" },
  toggle: { ios: { kind: "selection" }, android: "clock-tick" },
  confirmArm: { ios: { kind: "notification", type: "warning" }, android: "context-click" },
  success: { ios: { kind: "notification", type: "success" }, android: "confirm" },
  destructive: { ios: { kind: "impact", style: "heavy" }, android: "long-press" },
  error: { ios: { kind: "notification", type: "error" }, android: "reject" },
};

/** A mutation's haptics as `meta: { haptic }`, played by lib/queryClient.ts. A tagged failure plays "error" unless `error: false`. */
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

/** Null when unchanged: called from the worklet on every update. */
export function armHaptic(wasArmed: boolean, isArmed: boolean): HapticEvent | null {
  "worklet";
  if (wasArmed === isArmed) {
    return null;
  }
  return isArmed ? "swipeArmed" : "swipeDisarmed";
}
