import React from "react";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { exit } from "@tauri-apps/plugin-process";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { usePendingInvites } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import type { RouterContext } from "../types/router";

export const RootPage: React.FC = () => {
  const { currentUser } = useAppStore();
  const navigate = useNavigate();
  const router = useRouter();
  const { onLogout } = router.options.context as RouterContext;

  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingInvites = [] } = usePendingInvites();

  const items: TerminalMenuItem[] = [
    {
      id: "groups",
      label: "Groups",
      description: "Communities, Organizations, Teams, and overly-ambitious group chats",
      action: () => navigate({ to: "/groups" }),
      testId: "menu-item-groups",
    },
    {
      id: "dms",
      label: "Direct Messages",
      description: dmConversations.length > 0
        ? `${dmConversations.length} conversation${dmConversations.length !== 1 ? "s" : ""}`
        : "Start a new conversation",
      action: () => navigate({ to: "/dms" }),
      testId: "menu-item-dms",
    },
    {
      id: "invites",
      label: "Invites",
      description: pendingInvites.length > 0
        ? `${pendingInvites.length} pending`
        : "No pending invites",
      action: () => navigate({ to: "/invites" }),
      type: "system" as const,
      testId: "menu-item-invites",
    },
    { id: "__sep1__", label: "", type: "separator" },
    {
      id: "preferences",
      label: "Preferences",
      description: "Colors, font size, etc.",
      action: () => navigate({ to: "/preferences" }),
      type: "system",
      testId: "menu-item-preferences",
    },
    {
      id: "settings",
      label: "Settings",
      description: currentUser ? currentUser.email : undefined,
      action: () => navigate({ to: "/settings" }),
      type: "system",
      testId: "menu-item-settings",
    },
    { id: "__sep2__", label: "", type: "separator" },
    {
      id: "logout",
      label: "Log out",
      action: onLogout,
      type: "system",
      testId: "menu-item-logout",
    },
    {
      id: "exit",
      label: "Exit",
      action: () => exit(0),
      type: "system",
      testId: "menu-item-exit",
    },
  ];

  return <TerminalMenu items={items} />;
};
