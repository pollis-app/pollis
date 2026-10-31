import React, { useEffect, useRef } from "react";
import { useParams, useRouter } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { Phone, PhoneOff } from "lucide-react";
import { appStore } from "../stores/appStore";
import { historyNavStore } from "../stores/historyNavStore";
import { Avatar } from "../components/ui/Avatar";
import { Button } from "../components/ui/Button";
import { useOtherUserProfile } from "../hooks/queries/useUserProfile";
import { takeoverExit } from "../utils/incomingCallTakeover";
import {
  acceptIncomingCall,
  acceptedCalls,
  declineIncomingCall,
  markTakenOver,
  warmIncomingCall,
} from "../voice/incomingCall";

/** #1639: full-page "caller ID" for a ringing DM call. A route, not an overlay: leaving it returns the user one history step back. */
export const IncomingCallPage: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const router = useRouter();
  const { callId } = useParams({ from: "/incoming-call/$callId" });
  const ringing = appStore.incomingCall;
  const call = ringing && ringing.callId === callId ? ringing : null;
  const { data: profile } = useOtherUserProfile(call?.callerId ?? null);
  // One exit per screen: a cancel racing a decline must not step back twice.
  const exitedFor = useRef<string | null>(null);

  const ringingCallId = ringing?.callId ?? null;
  useEffect(() => {
    if (exitedFor.current === callId) {
      return;
    }
    const exit = takeoverExit(callId, ringingCallId, acceptedCalls());
    if (exit.kind === "stay" || exit.kind === "accepted") {
      return;
    }
    exitedFor.current = callId;
    if (exit.kind === "switch") {
      markTakenOver(exit.callId);
      router.navigate({
        to: "/incoming-call/$callId",
        params: { callId: exit.callId },
        replace: true,
      });
      return;
    }
    // Declined, cancelled by the caller, answered on another device, or a stale entry.
    if (historyNavStore.canGoBack) {
      router.history.back();
    } else {
      router.navigate({ to: "/", replace: true });
    }
  }, [callId, ringingCallId, router]);

  if (!call) {
    return <div data-testid="incoming-call-takeover-ended" className="h-full bg-bg" />;
  }

  const username = call.callerUsername;
  const displayName = profile?.preferred_name || `@${username}`;

  return (
    <div data-testid="incoming-call-takeover" className="flex flex-col h-full bg-bg">
      <div className="flex items-center px-4 flex-shrink-0 text-xs font-mono h-bar border-b border-line text-muted">
        <span className="text-accent flex-1">{t("takeover.title")}</span>
      </div>
      <div className="flex-1 flex flex-col items-center justify-center gap-6 px-6 py-8 overflow-auto">
        <Avatar
          avatarKey={profile?.avatar_url ?? null}
          size={112}
          variant="profile"
          alt={username}
        />
        <div className="flex flex-col items-center gap-1 text-center min-w-0 max-w-full">
          <h1
            data-testid="incoming-call-caller"
            className="text-2xl font-mono font-medium text-fg truncate max-w-full"
          >
            <bdi>{displayName}</bdi>
          </h1>
          <p className="text-sm font-mono text-dim flex items-center gap-2">
            <Phone className="w-4 h-4 text-accent status-bar-blink" aria-hidden />
            {profile?.preferred_name ? (
              <span>
                <bdi>@{username}</bdi> {t("takeover.calling")}
              </span>
            ) : (
              <span>{t("takeover.calling")}</span>
            )}
          </p>
        </div>
        <div className="flex items-center gap-4">
          <Button
            variant="danger"
            data-testid="incoming-call-decline"
            aria-label={t("takeover.declineAria", { username })}
            onClick={() => declineIncomingCall(call)}
          >
            <PhoneOff className="w-4 h-4" aria-hidden />
            {t("takeover.decline")}
          </Button>
          <Button
            variant="primary"
            data-testid="incoming-call-accept"
            aria-label={t("takeover.acceptAria", { username })}
            onPointerEnter={() => warmIncomingCall(call)}
            onFocus={() => warmIncomingCall(call)}
            onClick={() => acceptIncomingCall(router, call)}
          >
            <Phone className="w-4 h-4" aria-hidden />
            {t("takeover.accept")}
          </Button>
        </div>
      </div>
    </div>
  );
});
