import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Inbox, Ban, Plus } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useDMRequests } from "../hooks/queries";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";
import { Avatar } from "../components/ui/Avatar";

export const DMsPage: React.FC = () => {
  const navigate = useNavigate();
  const { setSelectedConversationId, markRead, unreadCounts } = useAppStore();

  const { data: conversations = [] } = useDMConversations();
  const { data: requests = [] } = useDMRequests();

  let items: TerminalMenuItem[] = [];

  items.push({
    id: "new-dm",
    label: "New Message",
    icon: <Plus size={14} />,
    action: () => navigate({ to: "/dms/new" }),
    type: "system" as const,
    testId: "menu-item-new-dm",
  });

  if (requests.length > 0) {
    items.push({
      id: "dm-requests",
      label: "Requests",
      icon: <Inbox size={14} />,
      action: () => navigate({ to: "/dms/requests" }),
      type: "system" as const,
      badge: requests.length,
      testId: "menu-item-dm-requests",
    });
  }

  items.push({
    id: "dm-blocked",
    label: "Blocked Users",
    icon: <Ban size={14} />,
    action: () => navigate({ to: "/dms/blocked" }),
    type: "system" as const,
    testId: "menu-item-dm-blocked",
  });

  if (conversations.length) {
    items.push({ id: "__sep__", label: "", type: "separator" as const });
    items = items.concat(
      conversations.map((c) => ({
        id: c.id,
        label: c.user2_identifier,
        icon: <Avatar avatarKey={c.user2_avatar_url} size={24} alt={`${c.user2_identifier} avatar`} testId={`dm-avatar-${c.id}`} />,
        iconChip: false,
        description: <LastMessagePreview conversationId={c.id} />,
        action: () => {
          setSelectedConversationId(c.id);
          markRead(c.id);
          navigate({ to: "/dms/$conversationId", params: { conversationId: c.id } });
        },
        badge: unreadCounts[c.id] ?? 0,
        testId: `dm-option-${c.id}`,
        secondaryAction: () => navigate({ to: "/dms/$conversationId/settings", params: { conversationId: c.id } }),
        secondaryActionLabel: `Settings for ${c.user2_identifier}`,
      })),
    );
  }

  items.push({
    id: "__back__",
    label: "Go back",
    icon: <ArrowLeft size={14} />,
    action: () => navigate({ to: "/" }),
    type: "system",
  });

  return (
    <TerminalMenu
      items={items}
      onEsc={() => navigate({ to: "/" })}
    />
  );
};
