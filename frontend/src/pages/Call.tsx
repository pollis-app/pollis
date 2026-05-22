import React, { useEffect } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PhoneOff } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../stores/appStore";
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

export const CallPage: React.FC = () => {
  const navigate = useNavigate();
  const { callId } = useParams({ from: "/call/$callId" });
  const roomName = `call-${callId}`;
  const activeVoiceChannelId = useAppStore((s) => s.activeVoiceChannelId);
  const voiceParticipants = useAppStore((s) => s.voiceParticipants);
  const outgoingCall = useAppStore((s) => s.outgoingCall);
  const setOutgoingCall = useAppStore((s) => s.setOutgoingCall);

  // Direct navigation to /call/<id> with no active voice → bounce back. Joining
  // is initiated by the caller's DM page or the callee's accept button; we
  // don't auto-join here because we have no caller/callee context for the
  // backend handshake.
  useEffect(() => {
    if (!activeVoiceChannelId) {
      navigate({ to: "/dms" });
    }
  }, [activeVoiceChannelId, navigate]);

  // Triggered by either the local hang-up button OR a `call_canceled` event
  // arriving on the inbox (the realtime handler clears activeVoiceChannelId
  // when it matches this call). Either way, leave the page.
  useEffect(() => {
    if (activeVoiceChannelId !== null && activeVoiceChannelId !== roomName) {
      // User joined a different voice channel while this call page was open —
      // step out so we don't sit on a stale call screen.
      navigate({ to: "/dms" });
    }
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
    return () => {
      const pending = useAppStore.getState().outgoingCall;
      if (pending && pending.callId === callId) {
        const calleeId = pending.calleeId;
        useAppStore.getState().setOutgoingCall(null);
        invoke("cancel_call", { otherUserId: calleeId, callId }).catch(() => {});
      }
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
            color: "var(--c-danger)",
            border: "2px solid var(--c-danger)",
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
};
