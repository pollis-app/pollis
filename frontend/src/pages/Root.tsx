import React from "react";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { exit } from "@tauri-apps/plugin-process";
import { Users, MessageCircle, Mail, UserPlus, Palette, User, ShieldCheck, LogOut, Power } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { usePendingInvites, useAllPendingJoinRequests } from "../hooks/queries/useGroups";
import { useDMConversations } from "../hooks/queries/useMessages";
import type { RouterContext } from "../types/router";

export const RootPage: React.FC = () => {
  const { currentUser, unreadCounts } = useAppStore();
  const navigate = useNavigate();
  const router = useRouter();
  const { onLogout } = router.options.context as RouterContext;

  const { data: dmConversations = [] } = useDMConversations();
  const { data: pendingInvites = [] } = usePendingInvites();
  const { data: pendingJoinRequests = [] } = useAllPendingJoinRequests();

  const totalDMUnread = dmConversations.reduce((sum, c) => sum + (unreadCounts[c.id] ?? 0), 0);

  const items: TerminalMenuItem[] = [
    {
      id: "groups",
      label: "Groups",
      icon: <Users size={14} />,
      description: "Communities, Organizations, Teams, and overly-ambitious group chats",
      action: () => navigate({ to: "/groups" }),
      testId: "menu-item-groups",
    },
    {
      id: "dms",
      label: "Direct Messages",
      icon: <MessageCircle size={14} />,
      description: "Private, end-to-end encrypted conversations with individuals",
      action: () => navigate({ to: "/dms" }),
      badge: totalDMUnread > 0 ? totalDMUnread : undefined,
      testId: "menu-item-dms",
    },
    {
      id: "invites",
      label: "Invites",
      icon: <Mail size={14} />,
      description: pendingInvites.length > 0
        ? <span className="status-bar-blink" style={{ color: "var(--c-accent)" }}>{pendingInvites.length} pending</span>
        : "No pending invites",
      action: () => navigate({ to: "/invites" }),
      type: "system" as const,
      testId: "menu-item-invites",
    },
    ...(pendingJoinRequests.length > 0 ? [{
      id: "join-requests",
      label: "Join Requests",
      icon: <UserPlus size={14} />,
      description: <span className="status-bar-blink" style={{ color: "var(--c-accent)" }}>{pendingJoinRequests.length} pending</span>,
      action: () => navigate({ to: "/join-requests" }),
      type: "system" as const,
      testId: "menu-item-join-requests",
    }] : []),
    { id: "__sep1__", label: "", type: "separator" },
    {
      id: "preferences",
      label: "Preferences",
      icon: <Palette size={14} />,
      description: "Colors, font size, etc.",
      action: () => navigate({ to: "/preferences" }),
      type: "system",
      testId: "menu-item-preferences",
    },
    {
      id: "settings",
      label: "Settings",
      icon: <User size={14} />,
      description: currentUser ? currentUser.email : undefined,
      action: () => navigate({ to: "/settings" }),
      type: "system",
      testId: "menu-item-settings",
    },
    {
      id: "security",
      label: "Security",
      icon: <ShieldCheck size={14} />,
      description: "Device enrollments, identity resets",
      action: () => navigate({ to: "/security" }),
      type: "system",
      testId: "menu-item-security",
    },
    { id: "__sep2__", label: "", type: "separator" },
    {
      id: "logout",
      label: "Log out",
      icon: <LogOut size={14} />,
      action: onLogout,
      type: "system",
      testId: "menu-item-logout",
    },
    {
      id: "exit",
      label: "Exit",
      icon: <Power size={14} />,
      action: () => exit(0),
      type: "system",
      testId: "menu-item-exit",
    },
  ];

  return <TerminalMenu items={items} />;
};
