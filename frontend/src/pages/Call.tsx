import React, { useEffect } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PhoneOff } from "lucide-react";
import { observer } from "mobx-react-lite";
import { invoke } from "../bridge";
import { appStore } from "../stores/appStore";
import { VoiceChannelView } from "../components/Voice/VoiceChannelView";
import { voiceSession } from "../voice";

/**
 * 1:1 call screen. Reuses the voice stack — a call is just a private LiveKit
 * room named `call-<call_id>`. Setting `activeVoiceChannelId` to the room name
 * causes `AppShell` to mount `VoiceBar`, which is what actually invokes
 * `join_voice_channel`. This page just renders the participant view and a
 * hang-up button on top.
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
  const activeVoiceChannelId =
    appStore.voiceState.kind === 'idle' ? null : appStore.voiceState.channelId;
  const voiceParticipants = appStore.voiceParticipants;
  const outgoingCall = appStore.outgoingCall;
  const setOutgoingCall = appStore.setOutgoingCall;

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

  const hangUp = () => {
    voiceSession.leave();
  };

  return (
    <div className="flex flex-col h-full font-mono text-xs">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0"
        style={{ borderBottom: "1px solid var(--c-border)", color: "var(--c-text-muted)" }}
      >
        <span style={{ flex: 1, color: "var(--c-accent)" }}>call</span>
      </div>

      <VoiceChannelView />

      <div
        className="px-4 py-3 flex items-center justify-end flex-shrink-0"
        style={{ borderTop: "1px solid var(--c-border)" }}
      >
        <button
          data-testid="call-hang-up"
          onClick={hangUp}
          className="inline-flex items-center gap-2"
          style={{
            background: "transparent",
            color: "#ff6b6b",
            border: "2px solid #ff6b6b",
            padding: "6px 14px",
            borderRadius: "0.25rem",
            cursor: "pointer",
            fontFamily: "inherit",
            fontSize: "inherit",
            fontWeight: "bold",
            letterSpacing: "0.05em",
          }}
        >
          <PhoneOff size={12} /> Hang up
        </button>
      </div>
    </div>
  );
});
