import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useDMConversations } from "../hooks/queries/useMessages";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";

export const DMsPage: React.FC = () => {
  const navigate = useNavigate();
  const { setSelectedConversationId, markRead, unreadCounts } = useAppStore();

  const { data: conversations = [] } = useDMConversations();

  let items: TerminalMenuItem[] = [];

  if (conversations.length) {
    items = items.concat([
      ...conversations.map((c) => ({
        id: c.id,
        label: c.user2_identifier,
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
      { id: "__sep__", label: "", type: "separator" as const },
    ]);
  }

  items = items.concat([
    {
      id: "new-dm",
      label: "New Message",
      action: () => navigate({ to: "/dms/new" }),
      type: "system" as const,
      testId: "menu-item-new-dm",
    },
    {
      id: "__back__",
      label: "Go back",
      icon: <ArrowLeft size={14} />,
      action: () => navigate({ to: "/" }),
      type: "system",
    },
  ]);

  return (
    <TerminalMenu
      items={items}
      onEsc={() => navigate({ to: "/" })}
    />
  );
};
