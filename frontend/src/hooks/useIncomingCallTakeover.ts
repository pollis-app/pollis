import { useEffect } from "react";
import { useRouter } from "@tanstack/react-router";
import type { IncomingCall } from "../stores/appStore";
import { appStore } from "../stores/appStore";
import { usePreferences } from "./queries/usePreferences";
import {
  INCOMING_CALL_TAKEOVER_DEFAULT,
  shouldTakeOver,
} from "../utils/incomingCallTakeover";
import { markTakenOver, takenOverCalls } from "../voice/incomingCall";

/** #1639: a ringing DM call replaces the screen with `/incoming-call/$callId`, unless the preference is off. */
export function useIncomingCallTakeover(incomingCall: IncomingCall | null): void {
  const router = useRouter();
  const { query } = usePreferences();
  const enabled = query.data?.incoming_call_takeover ?? INCOMING_CALL_TAKEOVER_DEFAULT;
  // Decide only once the preference is known, so "off" never flashes a takeover.
  const ready = query.data !== undefined || query.isError;
  const callId = incomingCall?.callId ?? null;
  const roomName = incomingCall?.roomName ?? null;

  useEffect(() => {
    if (!ready || !callId || !roomName) {
      return;
    }
    const voiceState = appStore.voiceState;
    const take = shouldTakeOver({
      enabled,
      ringing: { callId, roomName },
      pathname: router.state.location.pathname,
      activeVoiceChannelId: voiceState.kind === "idle" ? null : voiceState.channelId,
      takenOver: takenOverCalls(),
    });
    if (!take) {
      return;
    }
    markTakenOver(callId);
    // A push, so returning is one history step back to exactly where the user was.
    router.navigate({ to: "/incoming-call/$callId", params: { callId } });
  }, [ready, callId, roomName, enabled, router]);
}
