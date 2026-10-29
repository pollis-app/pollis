import React, { useEffect, useMemo } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import { VoiceStage } from "../components/Voice/stage/VoiceStage";
import { userIdFromVoiceIdentity } from "../voice/identity";
import { voiceSession } from "../voice";

/** 1:1 call screen: a private `call-<id>` LiveKit room rendered by the shared `VoiceStage` in `callMode`; owns ring timeout, bounce-back and cancel backstop. */
const RINGING_TIMEOUT_MS = 30_000;

// StrictMode and router churn re-mount transiently, so the backstop cancel waits DEFER_CANCEL_MS for a re-mount.
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

  // Counterparty name: the joined peer, else the ringing caller.
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

  // Bounce-back guard: our room stays, another room leaves, idle waits 1500ms since reconcile() runs a tick late.
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

  // Hang up after 30s alone, so an empty room isn't fed mic audio.
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

  // Once the callee joins, hang-up is a disconnect, not a ring cancel.
  useEffect(() => {
    if (hasRemoteParticipant && outgoingCall && outgoingCall.callId === callId) {
      setOutgoingCall(null);
    }
  }, [hasRemoteParticipant, outgoingCall, callId, setOutgoingCall]);

  // Backstop: cancel a still-pending outgoing call on unmount (read fresh from the store).
  useEffect(() => {
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
