import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { useAppStore } from "../stores/appStore";
import { useGroupMembers, useSetMemberRole, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";

export const MembersPage: React.FC = () => {
  const { groupId } = useParams({ from: "/groups/$groupId/members" });
  const navigate = useNavigate();
  const currentUser = useAppStore((state) => state.currentUser);
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const { data: members = [], isLoading } = useGroupMembers(groupId);
  const setRoleMutation = useSetMemberRole();

  if (!group) {
    return null;
  }
  const isAdmin = group.current_user_role === "admin";

  return (
    <PageShell title="Members" scrollable>
      <div className="pt-8 flex-1 flex flex-col overflow-hidden">
        <NavigableList
          items={members}
          isLoading={isLoading}
          emptyLabel="No members."
          testId="members-list"
          getKey={(m) => m.user_id}
          rowTestId={(m) => `member-row-${m.user_id}`}
          renderRow={(m) => {
            const isSelf = m.user_id === currentUser?.id;
            return (
              <span className="flex-1 truncate" style={{ color: "var(--c-text)" }}>
                {m.username ?? m.user_id}
                {isSelf && (
                  <span className="ml-2" style={{ color: "var(--c-text-muted)" }}>
                    (you)
                  </span>
                )}
              </span>
            );
          }}
          controls={(m) => {
            const isSelf = m.user_id === currentUser?.id;
            if (!isAdmin || isSelf) {
              return [];
            }
            return [
              <Switch
                id={`member-admin-toggle-${m.user_id}`}
                label="admin"
                checked={m.role === "admin"}
                onChange={() => {
                  const newRole = m.role === "admin" ? "member" : "admin";
                  setRoleMutation.mutate({ groupId, userId: m.user_id, role: newRole });
                }}
              />,
              <Button size="sm"
                data-testid={`member-kick-${m.user_id}`}
                variant="primary"
                onClick={() =>
                  navigate({
                    to: "/groups/$groupId/members/$userId/kick",
                    params: { groupId, userId: m.user_id },
                  })
                }
              >
                kick
              </Button>,
            ];
          }}
          trailing={(m) => {
            const isSelf = m.user_id === currentUser?.id;
            if (isAdmin && !isSelf) {
              return null;
            }
            return <span>{m.role}</span>;
          }}
        />
      </div>
    </PageShell>
  );
};
