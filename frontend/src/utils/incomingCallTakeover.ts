/* #1639: the full-page incoming-call takeover's decisions, kept pure so they are unit-tested. */

/** Synced preference key (the preferences blob); absent means on. */
export const INCOMING_CALL_TAKEOVER_PREF = "incoming_call_takeover";
export const INCOMING_CALL_TAKEOVER_DEFAULT = true;

const ROUTE_PREFIX = "/incoming-call/";

/** The takeover route for `callId`. */
export function incomingCallPath(callId: string): string {
  return `${ROUTE_PREFIX}${callId}`;
}

/** The call id a takeover pathname is for, else null. */
export function takeoverCallIdFromPath(pathname: string): string | null {
  if (!pathname.startsWith(ROUTE_PREFIX)) {
    return null;
  }
  const rest = pathname.slice(ROUTE_PREFIX.length);
  if (rest.length === 0 || rest.includes("/")) {
    return null;
  }
  return rest;
}

export interface TakeoverInput {
  /** The `incoming_call_takeover` preference. */
  enabled: boolean;
  /** The call ringing this device, if any. */
  ringing: { callId: string; roomName: string } | null;
  pathname: string;
  /** The voice room this device is in or joining, if any. */
  activeVoiceChannelId: string | null;
  /** Calls already taken over once: leaving the takeover must not drag the user back. */
  takenOver: ReadonlySet<string>;
}

/** Whether a ringing call should replace the current screen now. */
export function shouldTakeOver(input: TakeoverInput): boolean {
  const { enabled, ringing, pathname, activeVoiceChannelId, takenOver } = input;
  if (!enabled || !ringing) {
    return false;
  }
  if (takenOver.has(ringing.callId)) {
    return false;
  }
  if (takeoverCallIdFromPath(pathname) !== null) {
    return false;
  }
  if (pathname === `/call/${ringing.callId}`) {
    return false;
  }
  if (activeVoiceChannelId === ringing.roomName) {
    return false;
  }
  return true;
}

export type TakeoverExit =
  /** The call this screen shows is still ringing. */
  | { kind: "stay" }
  /** Answered: the accept already replaced this screen with the call. */
  | { kind: "accepted" }
  /** A different call now rings: show that one instead. */
  | { kind: "switch"; callId: string }
  /** Declined, cancelled by the caller, answered elsewhere, or stale: return the user. */
  | { kind: "back" };

/** What the takeover screen for `routeCallId` does given what is ringing now. */
export function takeoverExit(
  routeCallId: string,
  ringingCallId: string | null,
  accepted: ReadonlySet<string>,
): TakeoverExit {
  if (ringingCallId === routeCallId) {
    return { kind: "stay" };
  }
  if (accepted.has(routeCallId)) {
    return { kind: "accepted" };
  }
  if (ringingCallId !== null) {
    return { kind: "switch", callId: ringingCallId };
  }
  return { kind: "back" };
}
