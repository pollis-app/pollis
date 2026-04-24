import React, { useMemo } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Users, Plus, Search } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels, useAllPendingJoinRequests } from "../hooks/queries/useGroups";

export const GroupsPage: React.FC = () => {
  const navigate = useNavigate();
  const { setSelectedGroupId, unreadCounts } = useAppStore();

  const { data: groupsWithChannels, isLoading: groupsLoading, error: groupsError } = useUserGroupsWithChannels();
  const { data: allJoinRequests = [] } = useAllPendingJoinRequests();

  const groups = groupsWithChannels ?? [];

  // Build a map of groupId → pending join request count for badge display
  const joinRequestCountByGroup = useMemo(() => {
    const map: Record<string, number> = {};
    for (const req of allJoinRequests) {
      map[req.group_id] = (map[req.group_id] ?? 0) + 1;
    }
    return map;
  }, [allJoinRequests]);

  const groupItems: TerminalMenuItem[] = groupsLoading
    ? [{ id: "__loading__", label: "Loading…", disabled: true }]
    : groupsError
      ? [{ id: "__error__", label: `Error: ${groupsError instanceof Error ? groupsError.message : "Failed to load"}`, disabled: true }]
      : groups.map((g) => {
        const textChannels = g.channels.filter((ch) => ch.channel_type === "text");
        const totalUnread = textChannels.reduce((sum, ch) => sum + (unreadCounts[ch.id] ?? 0), 0);
        const pendingJoinCount = joinRequestCountByGroup[g.id] ?? 0;

        // Build description: prefer join request alert over static description text
        let description: React.ReactNode = g.description || undefined;
        if (pendingJoinCount > 0) {
          description = (
            <span className="status-bar-blink" style={{ color: "var(--c-accent)" }}>
              {pendingJoinCount} join request{pendingJoinCount !== 1 ? "s" : ""} pending
            </span>
          );
        }

        return {
          id: g.id,
          label: g.name,
          icon: <Users size={14} />,
          description,
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
      icon: <Plus size={14} />,
      action: () => navigate({ to: "/groups/new" }),
      type: "system",
      testId: "menu-item-create-group",
    },
    {
      id: "search-group",
      label: "Find Group",
      icon: <Search size={14} />,
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
