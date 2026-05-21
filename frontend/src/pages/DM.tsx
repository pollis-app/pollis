import React, { useEffect } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Phone } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useAppStore } from "../stores/appStore";
import { usePresenceStatus } from "../stores/presenceStore";
import { invoke } from "@tauri-apps/api/core";
import { voiceSession } from "../voice";

type RawDmMember = { user_id: string; username?: string; accepted_at?: string | null };
type RawDmChannel = { id: string; members: RawDmMember[] };

export const DMPage: React.FC = () => {
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId" });
  const setSelectedConversationId = useAppStore((s) => s.setSelectedConversationId);
  const currentUser = useAppStore((s) => s.currentUser);
  const setOutgoingCall = useAppStore((s) => s.setOutgoingCall);

  const [otherUserId, setOtherUserId] = React.useState<string | null>(null);
  const [memberCount, setMemberCount] = React.useState<number>(0);
  const [otherAcceptedAt, setOtherAcceptedAt] = React.useState<string | null>(null);

  useEffect(() => {
    setSelectedConversationId(conversationId);
    return () => { setSelectedConversationId(null); };
  }, [conversationId, setSelectedConversationId]);

  // Fetch member list for the conversation so we can target the right
  // user_id when blocking or calling, and discover whether the other party
  // has accepted this DM (gates the call button).
  useEffect(() => {
    let cancelled = false;
    if (!currentUser) {
      return;
    }
    invoke<RawDmChannel[]>("list_dm_channels", { userId: currentUser.id })
      .then((channels) => {
        if (cancelled) {
          return;
        }
        const match = channels.find((c) => c.id === conversationId);
        if (!match) {
          return;
        }
        setMemberCount(match.members.length);
        const other = match.members.find((m) => m.user_id !== currentUser.id);
        setOtherUserId(other?.user_id ?? null);
        setOtherAcceptedAt(other?.accepted_at ?? null);
      })
      .catch(() => { });
    return () => { cancelled = true; };
  }, [currentUser, conversationId]);

  const { data: conversations = [] } = useDMConversations();
  const conv = conversations.find((c) => c.id === conversationId);

  const username = conv?.user2_identifier ?? "";
  const isOneOnOne = memberCount === 2 && otherUserId != null;
  // Profile breadcrumb is enabled for 1:1 DMs only — group DMs (3+ members)
  // would need a picker.
  const canShowProfile = isOneOnOne;
  // Calling is only offered once the other party has accepted the DM, so an
  // unwanted DM request can never be escalated to a phone call.
  const otherPresence = usePresenceStatus(otherUserId);
  const isOtherOnline = otherPresence === "online";
  // Calls to an offline user fail at the LiveKit handshake (nobody to
  // create/answer the room), so only offer the call button when the peer
  // is online.
  const canCall = isOneOnOne && otherAcceptedAt !== null && isOtherOnline;

  const startCall = async () => {
    if (!currentUser || !otherUserId) {
      return;
    }
    try {
      const result = await invoke<{ call_id: string; room_name: string }>("start_call", {
        calleeId: otherUserId,
        callerId: currentUser.id,
        callerUsername: currentUser.username ?? currentUser.id,
      });
      // Record the outgoing call so the Call page can emit `cancel_call`
      // if the caller hangs up before the callee answers. Cleared once
      // the callee joins the LiveKit room or once cancel is sent.
      setOutgoingCall({ callId: result.call_id, calleeId: otherUserId });
      voiceSession.setIntent({
        channelId: result.room_name,
        groupId: null,
        counterpartyUserId: otherUserId,
      });
      navigate({ to: "/call/$callId", params: { callId: result.call_id } });
    } catch (err) {
      console.error("[call] start_call failed:", err);
    }
  };

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <span style={{ flex: 1 }}>
          {canShowProfile && username ? (
            <button
              data-testid="dm-header-username"
              onClick={() => navigate({ to: "/user/$userId", params: { userId: otherUserId! } })}
              className="font-mono transition-colors text-inherit hover:text-[var(--c-accent)]"
              style={{
                background: "none",
                border: "none",
                padding: 0,
                cursor: "pointer",
                fontSize: "inherit",
              }}
              aria-label={`View profile of @${username}`}
            >
              @{username}
            </button>
          ) : conv ? (
            `@${username}`
          ) : (
            "Direct Message"
          )}
        </span>
        {canCall && (
          <button
            data-testid="dm-header-call"
            onClick={startCall}
            aria-label={`Call @${username}`}
            className="icon-btn-sm flex-shrink-0"
            style={{
              position: 'absolute',
              right: '0.75rem'
            }}
          >
            <Phone size={14} aria-hidden="true" />
          </button>
        )}
      </div>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
};
