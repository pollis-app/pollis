import React, { useEffect, useMemo } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import { VoiceStage } from "../components/Voice/stage/VoiceStage";
import { userIdFromVoiceIdentity } from "../voice/identity";
import { voiceSession } from "../voice";

/**
 * 1:1 call screen. Reuses the voice stack — a call is just a private LiveKit
 * room named `call-<call_id>`. Setting `activeVoiceChannelId` to the room name
 * causes `AppShell` to mount `VoiceBar`, which is what actually invokes
 * `join_voice_channel`. This page renders the shared `VoiceStage` — the same
 * in-call surface a group voice channel uses (camera picker, self-preview,
 * remote camera + screen-share tiles, per-user volume, meters) — in `callMode`
 * so the two surfaces can't drift apart. This component only owns the
 * call-specific lifecycle (ringing timeout, bounce-back guard, cancel backstop).
 *
 * Reachable via two paths:
 * - Caller clicks the phone in a DM header → DM page invokes `start_call`,
 *   sets activeVoiceChannelId to `call-<id>`, and navigates here.
 * - Callee accepts an incoming-call alert in the bottom status bar → AppShell
 *   does the same.
 */
const RINGING_TIMEOUT_MS = 30_000;

// React.StrictMode double-invokes mount/unmount in dev, and TanStack Router
// can re-mount routes on state churn even in prod. Either way, an unmount
// is NOT a reliable signal that the user actually left the call — the same
// component may re-mount on the next tick. Buffer the backstop cancel: if
// the same callId re-mounts within DEFER_CANCEL_MS, the unmount was
// transient and we abort the cancel. Keyed by callId so two simultaneous
// Call pages (which shouldn't happen, but) don't collide.
const DEFER_CANCEL_MS = 200;
const pendingCancels = new Map<string, ReturnType<typeof setTimeout>>();

export const CallPage: React.FC = observer(() => {
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

  // Header title: the counterparty's display name when we can resolve it,
  // otherwise a generic "Call". Mirrors AppShell's VoiceBar name resolution —
  // prefer the joined peer's participant name, fall back to the incoming-call
  // slot's caller name while the peer is still ringing.
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
    return "Call";
  }, [voiceState, voiceParticipants, incomingCall]);

  // Bounce-back guard. Three cases:
  //   1. Voice is our room → stay.
  //   2. Voice is a different room → step out immediately (user joined a
  //      different voice channel while this page was open, or hangup from
  //      the other side cleared activeVoiceChannelId via `call_canceled`).
  //   3. Voice is idle → give `voiceSession.setIntent()` → `reconcile()` a
  //      moment to transition out of 'idle' before assuming we don't belong
  //      here. Both call entry points (caller in DM.tsx, callee in
  //      AppShell.tsx) call setIntent and `navigate()` synchronously, but
  //      reconcile() runs on the next tick — without the grace, the Call
  //      page mounts, sees `idle`, bounces back to /dms, and the cleanup
  //      effect below fires `cancel_call`, killing the callee's ring within
  //      ~100ms of the invite. 1500ms covers a typical 300–1000ms join.
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

  // Ringing timeout: if nobody else is in the room after 30s, auto-hang-up so
  // we stop publishing mic audio (and burning per-participant minutes) into
  // an empty room. Applies to both sides — caller waiting for an unanswered
  // ring, callee whose peer dropped before their join completed. The
  // voiceBridge `left` listener emits the `cancel_call` signal if this side
  // initiated the call and the callee never joined, so the recipient's ring
  // stops automatically here too.
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

  // Callee has joined the LiveKit room — the call is no longer pending, so
  // a subsequent hang-up is a normal disconnect, not a ring cancel.
  useEffect(() => {
    if (hasRemoteParticipant && outgoingCall && outgoingCall.callId === callId) {
      setOutgoingCall(null);
    }
  }, [hasRemoteParticipant, outgoingCall, callId, setOutgoingCall]);

  // Backstop: if this page unmounts while the outgoing call for this id is
  // still pending — e.g. join_voice_channel itself failed and no `left`
  // event ever fired — emit cancel so the callee's ring doesn't get stuck.
  // Reads through the store directly inside cleanup so we always see the
  // latest outgoingCall, not a stale closure capture.
  useEffect(() => {
    // If a deferred cancel from a previous (transient) unmount is queued
    // for this callId, abort it — we're back.
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
          invoke("cancel_call", { otherUserId: calleeId, callId }).catch(() => {});
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
      // A call has no pre-join roster and no "Join Voice" CTA.
      observerParticipants={[]}
      // callMode never renders a Join affordance, so onJoin is unreachable.
      onJoin={() => {}}
      // Leaving a call is a hang-up.
      onLeave={() => voiceSession.leave()}
      onBack={() => navigate({ to: "/dms" })}
      onOpenSettings={() => navigate({ to: "/voice-settings" })}
    />
  );
});
