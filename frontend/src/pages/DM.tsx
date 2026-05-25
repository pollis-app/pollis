import React, { useEffect, useMemo } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Phone, PhoneCall } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import type { PendingDmRequest } from "../components/Layout/MainContent";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useDMRequests } from "../hooks/queries";
import { useAppStore } from "../stores/appStore";
import { usePresenceStatus } from "../stores/presenceStore";
import { invoke } from "../bridge";
import { voiceSession } from "../voice";
import { KeyChangeBanner } from "../components/Security/KeyChangeBanner";
import { warmVoiceChannel } from "../utils/voiceWarmup";

type RawDmMember = { user_id: string; username?: string; accepted_at?: string | null };
type RawDmChannel = { id: string; members: RawDmMember[] };

export const DMPage: React.FC = () => {
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId" });
  const setSelectedConversationId = useAppStore((s) => s.setSelectedConversationId);
  const markRead = useAppStore((s) => s.markRead);
  const currentUser = useAppStore((s) => s.currentUser);
  const setOutgoingCall = useAppStore((s) => s.setOutgoingCall);
  const outgoingCall = useAppStore((s) => s.outgoingCall);

  const [otherUserId, setOtherUserId] = React.useState<string | null>(null);
  const [memberCount, setMemberCount] = React.useState<number>(0);
  const [otherAcceptedAt, setOtherAcceptedAt] = React.useState<string | null>(null);

  useEffect(() => {
    setSelectedConversationId(conversationId);
    // Same fix as ChannelPage: clear the unread badge on any nav path,
    // not just the DMs list click handler.
    markRead(conversationId);
    return () => { setSelectedConversationId(null); };
  }, [conversationId, setSelectedConversationId, markRead]);

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
  const { data: dmRequests = [] } = useDMRequests();
  const conv = conversations.find((c) => c.id === conversationId);
  // The DM is pending for the current user when it appears in the
  // requests list (their own `accepted_at` is NULL). Until they accept
  // (or block), the chat input is replaced with an accept/block bar.
  const pendingRequest = dmRequests.find((r) => r.id === conversationId) ?? null;
  const pendingSender = pendingRequest
    ? pendingRequest.members.find((m) => m.user_id !== currentUser?.id)
    : null;
  const username = conv?.user2_identifier
    ?? pendingSender?.username
    ?? pendingSender?.user_id
    ?? "";
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

  // Pre-warm DNS / TLS / connection pool to LiveKit as soon as a callable
  // DM is open. The token cache won't apply (the real `call-<id>` room
  // name isn't known until `start_call` returns) but the network plumbing
  // does, which is the dominant cost on a cold join.
  useEffect(() => {
    if (canCall && otherUserId) {
      warmVoiceChannel(`call-prewarm-${otherUserId}`);
    }
  }, [canCall, otherUserId]);

  // When the current user has not yet accepted this DM, the conversation
  // surfaces in `dmRequests` (filtered server-side by accepted_at IS NULL).
  // We hand the sender's identity to MainContent so it can render an
  // accept/block bar in place of the chat input.
  const pendingDmRequest: PendingDmRequest | null = useMemo(() => {
    if (!pendingRequest || !pendingSender) {
      return null;
    }
    return {
      senderUserId: pendingSender.user_id,
      senderName: pendingSender.username
        ? `@${pendingSender.username}`
        : pendingSender.user_id,
      onBlocked: () => {
        navigate({ to: "/dms/requests" });
      },
    };
  }, [pendingRequest, pendingSender, navigate]);

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
          ) : conv || pendingRequest ? (
            `@${username}`
          ) : (
            "Direct Message"
          )}
        </span>
        {canCall && (() => {
          // If there's a live outgoing 1:1 call to this exact user, the
          // button returns the user to the call room instead of starting
          // a new one. Visual + label both flip so it's obvious this is
          // a "rejoin" action, not "initiate".
          const inCallWithThisUser =
            outgoingCall != null && outgoingCall.calleeId === otherUserId;
          return (
            <button
              data-testid="dm-header-call"
              onClick={() => {
                if (inCallWithThisUser) {
                  navigate({
                    to: "/call/$callId",
                    params: { callId: outgoingCall.callId },
                  });
                } else {
                  void startCall();
                }
              }}
              onMouseEnter={() =>
                !inCallWithThisUser &&
                otherUserId &&
                warmVoiceChannel(`call-prewarm-${otherUserId}`)
              }
              aria-label={
                inCallWithThisUser
                  ? `Return to call with @${username}`
                  : `Call @${username}`
              }
              title={
                inCallWithThisUser
                  ? `Return to call with @${username}`
                  : `Call @${username}`
              }
              className="icon-btn-sm flex-shrink-0"
              style={{
                position: "absolute",
                right: "0.75rem",
                color: inCallWithThisUser ? "var(--c-accent)" : undefined,
              }}
            >
              {inCallWithThisUser ? (
                <PhoneCall size={14} aria-hidden="true" />
              ) : (
                <Phone size={14} aria-hidden="true" />
              )}
            </button>
          );
        })()}
      </div>
      <KeyChangeBanner peerUserId={otherUserId} peerLabel={username ? `@${username}` : undefined} />
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent pendingDmRequest={pendingDmRequest} />
      </div>
    </div>
  );
};
