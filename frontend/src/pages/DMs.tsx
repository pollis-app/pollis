import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Inbox, Ban, Plus, ShieldCheck, ShieldAlert } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useDMRequests } from "../hooks/queries";
import { usePeerVerifications } from "../hooks/queries/useUserProfile";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";

export const DMsPage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const navigate = useNavigate();
  const { setSelectedConversationId, markRead, unreadCounts } = appStore;

  const { data: conversations = [] } = useDMConversations();
  const { data: requests = [] } = useDMRequests();
  const { data: peerVerifications = [] } = usePeerVerifications();
  const verificationByPeer = useMemo(() => {
    const map = new Map<string, { verified: boolean; key_changed: boolean }>();
    for (const entry of peerVerifications) {
      map.set(entry.peer_user_id, {
        verified: entry.verified,
        key_changed: entry.key_changed,
      });
    }
    return map;
  }, [peerVerifications]);

  let items: TerminalMenuItem[] = [];

  items.push({
    id: "new-dm",
    label: t("list.newMessage"),
    icon: <Plus size={14} />,
    action: () => navigate({ to: "/dms/new" }),
    type: "system" as const,
    testId: "menu-item-new-dm",
  });

  if (requests.length > 0) {
    items.push({
      id: "dm-requests",
      label: t("list.requests"),
      icon: <Inbox size={14} />,
      action: () => navigate({ to: "/dms/requests" }),
      type: "system" as const,
      badge: requests.length,
      testId: "menu-item-dm-requests",
    });
  }

  items.push({
    id: "dm-blocked",
    label: t("list.blocked"),
    icon: <Ban size={14} />,
    action: () => navigate({ to: "/dms/blocked" }),
    type: "system" as const,
    testId: "menu-item-dm-blocked",
  });

  if (conversations.length) {
    items.push({ id: "__sep__", label: "", type: "separator" as const });
    items = items.concat(
      conversations.map((c) => {
        const verification = c.user2_id ? verificationByPeer.get(c.user2_id) : undefined;
        const trailingIndicator = verification?.key_changed ? (
          <ShieldAlert
            size={14}
            aria-label={t("list.keyChanged")}
            data-testid={`dm-verification-changed-${c.id}`}
            style={{ color: "#f0b429" }}
          />
        ) : verification?.verified ? (
          <ShieldCheck
            size={14}
            aria-label={t("list.verified")}
            data-testid={`dm-verification-verified-${c.id}`}
            style={{ color: "var(--c-accent)" }}
          />
        ) : undefined;
        return {
          id: c.id,
          label: c.user2_identifier,
          icon: (
            <PresenceAvatar
              userId={c.user2_id ?? null}
              avatarKey={c.user2_avatar_url}
              size={24}
              alt={t("list.avatarAlt", { name: c.user2_identifier })}
              testId={`dm-avatar-${c.id}`}
            />
          ),
          description: <LastMessagePreview conversationId={c.id} />,
          action: () => {
            setSelectedConversationId(c.id);
            markRead(c.id);
            navigate({ to: "/dms/$conversationId", params: { conversationId: c.id } });
          },
          badge: unreadCounts[c.id] ?? 0,
          trailingIndicator,
          testId: `dm-option-${c.id}`,
          secondaryAction: () => navigate({ to: "/dms/$conversationId/settings", params: { conversationId: c.id } }),
          secondaryActionLabel: t("list.settingsFor", { name: c.user2_identifier }),
        };
      }),
    );
  }

  items.push({
    id: "__back__",
    label: t("common:actions.goBack"),
    icon: <ArrowLeft size={14} className="rtl-mirror" />,
    action: () => navigate({ to: "/" }),
    type: "system",
  });

  return (
    <TerminalMenu
      items={items}
      onEsc={() => navigate({ to: "/" })}
    />
  );
});
