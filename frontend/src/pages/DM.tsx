import React, { useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Phone, PhoneCall, Pin, Shield, ShieldCheck } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useRightPanel } from "../components/Layout/RightPanel/useRightPanel";
import type { PendingDmRequest } from "../components/Layout/MainContent";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useDMRequests } from "../hooks/queries";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useMarkConversationRead } from "../hooks/queries/useUnread";
import { usePresenceStatus } from "../stores/presenceStore";
import { invoke } from "../bridge";
import { voiceSession } from "../voice";
import { usePeerVerificationMap } from "../hooks/queries/useUserProfile";
import { useReadReceipts } from "../hooks/useReadReceipts";
import { useSendReadReceipts } from "../hooks/queries/usePreferences";
import { warmVoiceChannel } from "../utils/voiceWarmup";
import { PressButton } from "../components/ui/PressButton";
import { PillButton } from "../components/ui/PillButton";
import { ConversationHeader } from "../components/Layout/ConversationHeader";
import { activeCallIdOf, voiceCounterpartyUserId } from "../types/voice-state";

type RawDmMember = { user_id: string; username?: string; accepted_at?: string | null };
type RawDmChannel = { id: string; members: RawDmMember[] };

export const DMPage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId" });
  const setSelectedConversationId = appStore.setSelectedConversationId;
  const { mutate: markConversationRead } = useMarkConversationRead(appStore.currentUser?.id ?? null);
  const currentUser = appStore.currentUser;
  const setOutgoingCall = appStore.setOutgoingCall;
  // Joining or in a 1:1 call, ringing or answered, either side (#1640).
  const activeCallId = activeCallIdOf(appStore.voiceState);
  const callPeerId = voiceCounterpartyUserId(appStore.voiceState);

  const sendReadReceipts = useSendReadReceipts();

  const [otherUserId, setOtherUserId] = React.useState<string | null>(null);
  const [memberCount, setMemberCount] = React.useState<number>(0);
  const [otherAcceptedAt, setOtherAcceptedAt] = React.useState<string | null>(null);

  useEffect(() => {
    setSelectedConversationId(conversationId);
    // Clear on any nav path, with a durable cursor (#844).
    markConversationRead(conversationId);
    return () => { setSelectedConversationId(null); };
  }, [conversationId, setSelectedConversationId, markConversationRead]);

  // Receipts (#857) are DM-only and track their own viewport + focus; Rust enforces the preference.
  useReadReceipts(conversationId, currentUser?.id ?? null, sendReadReceipts);

  // Members, for block/call targets and whether the peer accepted.
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
      .catch((e) => console.warn("list_dm_channels failed", e));
    return () => { cancelled = true; };
  }, [currentUser, conversationId]);

  const { data: conversations = [] } = useDMConversations();
  const { data: dmRequests = [] } = useDMRequests();
  const conv = conversations.find((c) => c.id === conversationId);
  // Pending for us until accepted (or blocked).
  const pendingRequest = dmRequests.find((r) => r.id === conversationId) ?? null;
  const pendingSender = pendingRequest
    ? pendingRequest.members.find((m) => m.user_id !== currentUser?.id)
    : null;
  const username = conv?.user2_identifier
    ?? pendingSender?.username
    ?? pendingSender?.user_id
    ?? "";
  const isOneOnOne = memberCount === 2 && otherUserId != null;
  // 1:1 only; group DMs would need a picker.
  const canShowProfile = isOneOnOne;
  // Only after the peer accepted, so a request can't escalate to a call.
  const otherPresence = usePresenceStatus(otherUserId);
  const isOtherOnline = otherPresence === "online";
  // Offline peers fail at the handshake.
  const canCall = isOneOnOne && otherAcceptedAt !== null && isOtherOnline;
  const { togglePins, setSafety } = useRightPanel();
  const verificationByPeer = usePeerVerificationMap();
  const peerVerified = otherUserId ? verificationByPeer.get(otherUserId)?.verified === true : false;

  // Pre-warm the LiveKit connection for a callable DM.
  useEffect(() => {
    if (canCall && otherUserId) {
      warmVoiceChannel(`call-prewarm-${otherUserId}`);
    }
  }, [canCall, otherUserId]);

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
      // So the Call page can `cancel_call` if we hang up before pickup.
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

  const returnToCallLabel = callPeerId != null && callPeerId === otherUserId
    ? t("conversation.returnToCall", { name: username })
    : t("voice:bar.returnToCall");

  const title = canShowProfile && username ? (
    <PressButton
      data-testid="dm-header-username"
      onClick={() => navigate({ to: "/user/$userId", params: { userId: otherUserId! } })}
      className="font-mono transition-colors text-inherit hover:text-accent border-0 bg-transparent p-0 cursor-pointer text-xs"
      aria-label={t("conversation.viewProfile", { name: username })}
    >
      <bdi>@{username}</bdi>
    </PressButton>
  ) : conv || pendingRequest ? (
    `@${username}`
  ) : (
    t("conversation.fallbackTitle")
  );

  return (
    <div className="flex flex-col h-full">
      <ConversationHeader title={title}>
        {isOneOnOne && otherUserId && (
          // Verified only while `verified = 1`; a key change clears it, the timeline says why (#1605).
          <PressButton
            data-testid={peerVerified ? "dm-header-verified" : "dm-header-unverified"}
            onClick={() => setSafety(otherUserId)}
            aria-label={peerVerified ? t("list.verified") : t("safety.verify")}
            title={peerVerified ? t("list.verified") : t("safety.verify")}
            className={`icon-btn-sm flex-shrink-0 ${peerVerified ? "text-accent" : ""}`}
          >
            {peerVerified ? (
              <ShieldCheck size={14} aria-hidden="true" />
            ) : (
              <Shield size={14} aria-hidden="true" />
            )}
          </PressButton>
        )}
        <PressButton
          data-testid="dm-pins-trigger"
          onClick={togglePins}
          aria-label={t("conversation.pinsLabel")}
          title={t("conversation.pinsLabel")}
          className="icon-btn-sm flex-shrink-0"
        >
          <Pin size={14} aria-hidden="true" />
        </PressButton>
        {activeCallId ? (
          // In a 1:1 call: the call slot becomes the voice bar's "return to call" (#1640).
          <PillButton
            data-testid="dm-header-return-to-call"
            accent="var(--c-accent)"
            onClick={() => navigate({ to: "/call/$callId", params: { callId: activeCallId } })}
            title={returnToCallLabel}
            aria-label={returnToCallLabel}
          >
            <PhoneCall size={12} aria-hidden="true" />
            {t("voice:bar.returnToCall")}
          </PillButton>
        ) : canCall ? (
          <button
            data-testid="dm-header-call"
            onClick={() => {
              void startCall();
            }}
            onMouseEnter={() => {
              if (otherUserId) {
                warmVoiceChannel(`call-prewarm-${otherUserId}`);
              }
            }}
            aria-label={t("conversation.call", { name: username })}
            title={t("conversation.call", { name: username })}
            className="icon-btn-sm flex-shrink-0"
          >
            <Phone size={14} aria-hidden="true" />
          </button>
        ) : null}
      </ConversationHeader>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent pendingDmRequest={pendingDmRequest} />
      </div>
    </div>
  );
});
