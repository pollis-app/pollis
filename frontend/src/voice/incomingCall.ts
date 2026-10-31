import { invoke } from "../bridge";
import { appStore, type IncomingCall } from "../stores/appStore";
import type { AppRouter } from "../router";
import { takeoverCallIdFromPath } from "../utils/incomingCallTakeover";
import { warmVoiceChannel } from "../utils/voiceWarmup";
import { voiceSession } from "./VoiceSessionManager";

/* Answer / decline for a ringing 1:1 call, shared by the status-bar alert and the takeover screen (#1639). */

// Calls answered on this device: the takeover must not "return" the user once its call is accepted.
const acceptedCallIds = new Set<string>();
// Calls whose takeover has been shown once; leaving it never brings it back.
const takenOverCallIds = new Set<string>();

export function acceptedCalls(): ReadonlySet<string> {
  return acceptedCallIds;
}

export function takenOverCalls(): ReadonlySet<string> {
  return takenOverCallIds;
}

export function markTakenOver(callId: string): void {
  takenOverCallIds.add(callId);
}

function dismissOnMyDevices(callId: string): void {
  const user = appStore.currentUser;
  if (!user) {
    return;
  }
  // Stop ringing on other devices; `call_canceled` is idempotent.
  invoke("dismiss_call_on_my_devices", { userId: user.id, callId }).catch((e) =>
    console.warn("dismiss_call_on_my_devices failed", e),
  );
}

/** Pre-warm the call room (token + DNS/TLS) on intent to answer, as group voice rows do (#176). */
export function warmIncomingCall(call: IncomingCall): void {
  warmVoiceChannel(call.roomName);
}

/** Answer: route to the call (in place of the takeover, if it is showing), then join. */
export function acceptIncomingCall(router: AppRouter, call: IncomingCall): void {
  const { callId } = call;
  acceptedCallIds.add(callId);
  const onTakeover = takeoverCallIdFromPath(router.state.location.pathname) !== null;
  // Route first so the old voice page unmounts before the room swaps.
  router.navigate({ to: "/call/$callId", params: { callId }, replace: onTakeover });
  voiceSession.setIntent({
    channelId: call.roomName,
    groupId: null,
    counterpartyUserId: call.callerId,
  });
  appStore.setIncomingCall(null);
  dismissOnMyDevices(callId);
}

/** Decline: tell the caller, stop every device ringing. The takeover returns the user itself. */
export function declineIncomingCall(call: IncomingCall): void {
  const { callId, callerId } = call;
  appStore.setIncomingCall(null);
  invoke("cancel_call", { otherUserId: callerId, callId }).catch((e) =>
    console.warn("cancel_call failed", e),
  );
  dismissOnMyDevices(callId);
}
