import { errorMessage } from "../utils/errorMessage";
import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Users, Plus, Search } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useUserGroupsWithChannels, useAllPendingJoinRequests } from "../hooks/queries/useGroups";

export const GroupsPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { setSelectedGroupId } = appStore;

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
    ? [{ id: "__loading__", label: t("common:states.loading"), disabled: true }]
    : groupsError
      ? [
          {
            id: "__error__",
            label: t("groups.loadError", {
              message: errorMessage(groupsError, t("groups.loadFailed")),
            }),
            disabled: true,
          },
        ]
      : groups.map((g) => {
        const textChannels = g.channels.filter((ch) => ch.channel_type === "text");
        const totalUnread = appStore.unreadFor(textChannels);
        const pendingJoinCount = joinRequestCountByGroup[g.id] ?? 0;

        // Build description: prefer join request alert over static description text
        let description: React.ReactNode = g.description || undefined;
        if (pendingJoinCount > 0) {
          description = (
            <span className="status-bar-blink text-accent">
              {t("groups.joinRequestsPending", { count: pendingJoinCount })}
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

  let items: TerminalMenuItem[] = [
    {
      id: "create-group",
      label: t("groups.create"),
      icon: <Plus size={14} />,
      action: () => navigate({ to: "/groups/new" }),
      type: "system",
      testId: "menu-item-create-group",
    },
    {
      id: "search-group",
      label: t("groups.find"),
      icon: <Search size={14} />,
      action: () => navigate({ to: "/groups/search" }),
      type: "system",
      testId: "menu-item-find-group",
    },
  ];

  if (groupItems.length) {
    items.push({ id: "__sep__", label: "", type: "separator" });
    items = items.concat(groupItems);
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
