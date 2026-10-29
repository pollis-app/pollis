import React, { useEffect, useMemo } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import { VoiceStage } from "../components/Voice/stage/VoiceStage";
import { userIdFromVoiceIdentity } from "../voice/identity";
import { voiceSession } from "../voice";

/**
 * 1:1 call screen: a private LiveKit room `call-<call_id>`, joined by setting
 * `activeVoiceChannelId` (AppShell's VoiceBar then joins). Renders the shared
 * `VoiceStage` in `callMode`; this owns only the ringing timeout, bounce-back
 * guard and cancel backstop. Reached from the DM header (caller) or the
 * incoming-call alert (callee).
 */
const RINGING_TIMEOUT_MS = 30_000;

// An unmount isn't proof the user left (StrictMode, router re-mounts), so the
// backstop cancel is deferred; a re-mount of the same callId within
// DEFER_CANCEL_MS aborts it.
const DEFER_CANCEL_MS = 200;
const pendingCancels = new Map<string, ReturnType<typeof setTimeout>>();

export const CallPage: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const navigate = useNavigate();
  const { callId } = useParams({ from: "/call/$callId" });
  const roomName = `call-${callId}`;
  const voiceState = appStore.voiceState;
  const activeVoiceChannelId =
    voiceState.kind === 'idle' ? null : voiceState.channelId;
  const voiceParticipants = appStore.voiceParticipants;
  const incomingCall = appStore.incomingCall;
  const outgoingCall = appStore.outgoingCall;
  const setOutgoingCall = appStore.setOutgoingCall;

  // The counterparty's name if resolvable (joined peer, else the ringing
  // caller), else "Call".
  const channelName = useMemo(() => {
    const peerId =
      voiceState.kind === "joined" || voiceState.kind === "joining"
        ? voiceState.counterpartyUserId
        : null;
    if (peerId) {
      const peer = voiceParticipants.find(
        (p) => userIdFromVoiceIdentity(p.identity) === peerId,
      );
      if (peer?.name) {
        return peer.name;
      }
      if (incomingCall && incomingCall.callerId === peerId) {
        return incomingCall.callerUsername;
      }
    }
    return t("call.defaultTitle");
  }, [voiceState, voiceParticipants, incomingCall, t]);

  // Bounce-back guard: our room → stay; another room → leave now; idle →
  // wait 1500ms, because `reconcile()` runs a tick after `setIntent()` and
  // bouncing immediately would fire `cancel_call` and kill the callee's ring.
  useEffect(() => {
    if (activeVoiceChannelId === roomName) {
      return;
    }
    if (activeVoiceChannelId !== null) {
      navigate({ to: "/dms" });
      return;
    }
    const timer = setTimeout(() => {
      const cur = appStore.voiceState;
      const curChannel = cur.kind === 'idle' ? null : cur.channelId;
      if (curChannel !== roomName) {
        navigate({ to: "/dms" });
      }
    }, 1500);
    return () => clearTimeout(timer);
  }, [activeVoiceChannelId, roomName, navigate]);

  // Hang up after 30s with nobody else in the room (both sides), so we stop
  // publishing into an empty room; voiceBridge's `left` sends `cancel_call`.
  const hasRemoteParticipant = voiceParticipants.some((p) => !p.isLocal);
  useEffect(() => {
    if (hasRemoteParticipant || activeVoiceChannelId !== roomName) {
      return;
    }
    const timer = setTimeout(() => {
      voiceSession.leave();
    }, RINGING_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [hasRemoteParticipant, activeVoiceChannelId, roomName]);

  // The callee joined: a later hang-up is a normal disconnect.
  useEffect(() => {
    if (hasRemoteParticipant && outgoingCall && outgoingCall.callId === callId) {
      setOutgoingCall(null);
    }
  }, [hasRemoteParticipant, outgoingCall, callId, setOutgoingCall]);

  // Backstop: unmounting while our outgoing call is still pending (e.g. the
  // join failed and no `left` fired) emits cancel. Reads the store at cleanup.
  useEffect(() => {
    // Abort a deferred cancel from a transient unmount: we're back.
    const queued = pendingCancels.get(callId);
    if (queued) {
      clearTimeout(queued);
      pendingCancels.delete(callId);
    }
    return () => {
      const timer = setTimeout(() => {
        pendingCancels.delete(callId);
        const pending = appStore.outgoingCall;
        if (pending && pending.callId === callId) {
          const calleeId = pending.calleeId;
          appStore.setOutgoingCall(null);
          invoke("cancel_call", { otherUserId: calleeId, callId }).catch((e) => console.warn("cancel_call failed", e));
        }
      }, DEFER_CANCEL_MS);
      pendingCancels.set(callId, timer);
    };
  }, [callId]);

  return (
    <VoiceStage
      callMode
      channelName={channelName}
      isInCall={activeVoiceChannelId === roomName}
      observerParticipants={[]}
      onJoin={() => {}}
      onLeave={() => voiceSession.leave()}
      onBack={() => navigate({ to: "/dms" })}
      onOpenSettings={() => navigate({ to: "/voice-settings" })}
    />
  );
});
