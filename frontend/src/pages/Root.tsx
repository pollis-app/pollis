import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { exit } from "../bridge";
import { Users, MessageCircle, Mail, UserPlus, LogOut, Power, Link as LinkIcon } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { usePendingInvites, useAllPendingJoinRequests } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import type { RouterContext } from "../types/router";

export const RootPage: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  const navigate = useNavigate();
  const router = useRouter();
  const { onLogout } = router.options.context as RouterContext;

  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingInvites = [] } = usePendingInvites();
  const { data: pendingJoinRequests = [] } = useAllPendingJoinRequests();

  const totalDMUnread = appStore.unreadFor(dmConversations);

  const items: TerminalMenuItem[] = [
    {
      id: "groups",
      label: t("home.groups"),
      icon: <Users size={14} />,
      description: t("home.groupsDescription"),
      action: () => navigate({ to: "/groups" }),
      testId: "menu-item-groups",
    },
    {
      id: "dms",
      label: t("home.dms"),
      icon: <MessageCircle size={14} />,
      description: t("home.dmsDescription"),
      action: () => navigate({ to: "/dms" }),
      badge: totalDMUnread > 0 ? totalDMUnread : undefined,
      testId: "menu-item-dms",
    },
    {
      id: "invites",
      label: t("home.invites"),
      icon: <Mail size={14} />,
      description: pendingInvites.length > 0
        ? <span className="status-bar-blink text-accent">{t("home.pending", { count: pendingInvites.length })}</span>
        : t("home.noPendingInvites"),
      action: () => navigate({ to: "/invites" }),
      type: "system" as const,
      testId: "menu-item-invites",
    },
    {
      id: "join",
      label: t("home.join"),
      icon: <LinkIcon size={14} />,
      description: t("home.joinDescription"),
      action: () => navigate({ to: "/join" }),
      type: "system" as const,
      testId: "menu-item-join",
    },
    ...(pendingJoinRequests.length > 0 ? [{
      id: "join-requests",
      label: t("home.joinRequests"),
      icon: <UserPlus size={14} />,
      description: <span className="status-bar-blink text-accent">{t("home.pending", { count: pendingJoinRequests.length })}</span>,
      action: () => navigate({ to: "/join-requests" }),
      type: "system" as const,
      testId: "menu-item-join-requests",
    }] : []),
    { id: "__sep1__", label: "", type: "separator" },
    {
      id: "logout",
      label: t("home.logout"),
      icon: <LogOut size={14} />,
      action: onLogout,
      type: "system",
      testId: "menu-item-logout",
    },
    {
      id: "exit",
      label: t("home.exit"),
      icon: <Power size={14} />,
      action: () => exit(0),
      type: "system",
      testId: "menu-item-exit",
    },
  ];

  return <TerminalMenu items={items} />;
});
