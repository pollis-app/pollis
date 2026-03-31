import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const GroupsPage: React.FC = () => {
  const navigate = useNavigate();
  const { setSelectedGroupId, unreadCounts } = useAppStore();

  const { data: groupsWithChannels, isLoading: groupsLoading, error: groupsError } = useUserGroupsWithChannels();

  const groups = groupsWithChannels ?? [];

  const groupItems: TerminalMenuItem[] = groupsLoading
    ? [{ id: "__loading__", label: "Loading…", disabled: true }]
    : groupsError
      ? [{ id: "__error__", label: `Error: ${groupsError instanceof Error ? groupsError.message : "Failed to load"}`, disabled: true }]
      : groups.map((g) => {
        const textChannels = g.channels.filter((ch) => ch.channel_type === "text");
        const totalUnread = textChannels.reduce((sum, ch) => sum + (unreadCounts[ch.id] ?? 0), 0);
        return {
          id: g.id,
          label: g.name,
          description: g.description || undefined,
          action: () => {
            setSelectedGroupId(g.id);
            navigate({ to: "/groups/$groupId", params: { groupId: g.id } });
          },
          badge: totalUnread > 0 ? totalUnread : undefined,
          testId: `group-option-${g.id}`,
        };
      });

  let items: TerminalMenuItem[] = [];

  if (groupItems.length) {
    items = items.concat([
      ...groupItems,
      { id: "__sep__", label: "", type: "separator" },
    ]);
  }

  items = items.concat([
    {
      id: "create-group",
      label: "Create Group",
      action: () => navigate({ to: "/groups/new" }),
      type: "system",
      testId: "menu-item-create-group",
    },
    {
      id: "search-group",
      label: "Find Group",
      action: () => navigate({ to: "/groups/search" }),
      type: "system",
      testId: "menu-item-find-group",
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
