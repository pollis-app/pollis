import React, { useMemo } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ShieldCheck, ShieldAlert } from "lucide-react";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useGroupMembers, useSetMemberRole } from "../hooks/queries/useGroups";
import { usePeerVerifications } from "../hooks/queries/useUserProfile";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";

interface MembersProps {
  groupId: string;
  isAdmin: boolean;
}

export const Members: React.FC<MembersProps> = observer(({ groupId, isAdmin }) => {
  const navigate = useNavigate();
  const currentUser = appStore.currentUser;
  const { data: members = [], isLoading } = useGroupMembers(groupId);
  const setRoleMutation = useSetMemberRole();
  const { data: peerVerifications = [] } = usePeerVerifications();
  // peerUserId → { verified, key_changed }. Reuses the same query the DM
  // sidebar already loads, so the badge state is consistent across every
  // surface where the same person appears (DM, group, channel author).
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

  return (
    <NavigableList
      items={members}
      isLoading={isLoading}
      emptyLabel="No members."
      testId="members-list"
      getKey={(m) => m.user_id}
      rowTestId={(m) => `member-row-${m.user_id}`}
      onClickRow={(m) => {
        const isSelf = m.user_id === currentUser?.id;
        if (isSelf) {
          return;
        }
        navigate({ to: "/user/$userId", params: { userId: m.user_id } });
      }}
      onEnterRow={(m) => {
        const isSelf = m.user_id === currentUser?.id;
        if (isSelf) {
          return;
        }
        navigate({ to: "/user/$userId", params: { userId: m.user_id } });
      }}
      renderRow={(m) => {
        const isSelf = m.user_id === currentUser?.id;
        // Verified badge wins; a `key_changed` mismatch overrides it
        // (matches the DM sidebar logic — see Sidebar.tsx).
        const verification = verificationByPeer.get(m.user_id);
        const badge = isSelf ? null : verification?.key_changed ? (
          <span
            data-testid={`member-verification-changed-${m.user_id}`}
            title="Identity key changed — re-verify"
            style={{ display: "inline-flex", color: "#f0b429", flexShrink: 0 }}
          >
            <ShieldAlert size={14} />
          </span>
        ) : verification?.verified ? (
          <span
            data-testid={`member-verification-verified-${m.user_id}`}
            title="Verified contact"
            style={{ display: "inline-flex", color: "var(--c-accent)", flexShrink: 0 }}
          >
            <ShieldCheck size={14} />
          </span>
        ) : null;
        return (
          <span
            className="flex-1 truncate flex items-center gap-2"
            style={{ color: "var(--c-text)" }}
          >
            <span className="truncate">{m.username ?? m.user_id}</span>
            {badge}
            {isSelf && (
              <span className="ml-1" style={{ color: "var(--c-text-muted)" }}>
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
  );
});
