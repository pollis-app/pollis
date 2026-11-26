import { useObserver } from "mobx-react-lite";
import { appStore } from "../../../stores/appStore";
import { useDMConversations } from "../../../hooks/queries/useMessages";
import { voiceCounterpartyUserId } from "../../../types/voice-state";
import { callIdFromPath, resolveCallDm } from "../../../utils/callDm";

/** On the 1:1 call page, the DM with the other party (#2116), or null when there is none (no DM is created for a call). */
export function useCallDm(pathname: string): string | null {
  const callId = callIdFromPath(pathname);
  const peerId = useObserver(() => {
    if (!callId) {
      return null;
    }
    // Joining or joined, either side; else the ringing caller or the callee we're ringing.
    const counterparty = voiceCounterpartyUserId(appStore.voiceState);
    if (counterparty) {
      return counterparty;
    }
    if (appStore.incomingCall?.callId === callId) {
      return appStore.incomingCall.callerId;
    }
    if (appStore.outgoingCall?.callId === callId) {
      return appStore.outgoingCall.calleeId;
    }
    return null;
  });
  const { data: conversations = [] } = useDMConversations();
  return resolveCallDm(conversations, peerId);
}
