import React, { useState, useEffect, useCallback, useRef } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useAppStore } from "../stores/appStore";
import { useGroupMembers, useSetMemberRole } from "../hooks/queries/useGroups";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";

// colIndex: 0 = name (row focus), 1 = admin toggle, 2 = kick button
type NavState = { rowIndex: number; colIndex: number };

interface MembersProps {
  groupId: string;
  isAdmin: boolean;
}

export const Members: React.FC<MembersProps> = ({ groupId, isAdmin }) => {
  const navigate = useNavigate();
  const currentUser = useAppStore((state) => state.currentUser);
  const { data: allMembers = [], isLoading } = useGroupMembers(groupId);
  const setRoleMutation = useSetMemberRole();

  const members = allMembers;

  const [nav, setNav] = useState<NavState>({ rowIndex: 0, colIndex: 0 });
  const containerRef = useRef<HTMLDivElement>(null);

  const maxCol = isAdmin ? 2 : 0;

  // Move DOM focus to the right element whenever nav changes.
  // colIndex 0 → container div (shows row highlight).
  // colIndex 1 → the Switch button in the focused row (native focus ring).
  // colIndex 2 → the kick Button in the focused row (native focus ring).
  useEffect(() => {
    if (members.length === 0) {
      return;
    }
    const member = members[nav.rowIndex];
    if (!member) {
      return;
    }
    const isSelf = member.user_id === currentUser?.id;

    if (nav.colIndex === 0 || (isSelf && nav.colIndex > 0)) {
      containerRef.current?.focus();
      return;
    }

    const rowEl = containerRef.current?.querySelector<HTMLElement>(
      `[data-testid="member-row-${member.user_id}"]`,
    );
    if (!rowEl) {
      return;
    }

    if (nav.colIndex === 1) {
      rowEl.querySelector<HTMLElement>('[role="switch"]')?.focus();
    } else if (nav.colIndex === 2) {
      rowEl.querySelector<HTMLElement>('[data-testid^="member-kick-"]')?.focus();
    }
  }, [nav, members, currentUser?.id]);

  // Focus the container once members are loaded so keyboard nav works immediately.
  useEffect(() => {
    if (!isLoading && members.length > 0) {
      containerRef.current?.focus();
    }
  }, [isLoading, members.length]);

  useEffect(() => {
    setNav({ rowIndex: 0, colIndex: 0 });
  }, [members.length]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (members.length === 0) {
        return;
      }
      switch (e.key) {
        case "ArrowUp": {
          e.preventDefault();
          setNav((prev) => ({
            rowIndex: prev.rowIndex > 0 ? prev.rowIndex - 1 : members.length - 1,
            colIndex: 0,
          }));
          break;
        }
        case "ArrowDown": {
          e.preventDefault();
          setNav((prev) => ({
            rowIndex: prev.rowIndex < members.length - 1 ? prev.rowIndex + 1 : 0,
            colIndex: 0,
          }));
          break;
        }
        case "ArrowRight": {
          if (!isAdmin) {
            break;
          }
          e.preventDefault();
          setNav((prev) => ({
            ...prev,
            colIndex: prev.colIndex < maxCol ? prev.colIndex + 1 : maxCol,
          }));
          break;
        }
        case "ArrowLeft": {
          if (!isAdmin) {
            break;
          }
          e.preventDefault();
          setNav((prev) => ({
            ...prev,
            colIndex: prev.colIndex > 0 ? prev.colIndex - 1 : 0,
          }));
          break;
        }
        case "Enter": {
          if (!isAdmin) {
            break;
          }
          e.preventDefault();
          const member = members[nav.rowIndex];
          if (!member) {
            break;
          }
          // Skip controls for own row
          if (member.user_id === currentUser?.id) {
            break;
          }
          if (nav.colIndex === 1) {
            const newRole = member.role === "admin" ? "member" : "admin";
            setRoleMutation.mutate({ groupId, userId: member.user_id, role: newRole });
          } else if (nav.colIndex === 2) {
            navigate({
              to: "/groups/$groupId/members/$userId/kick",
              params: { groupId, userId: member.user_id },
            });
          }
          break;
        }
        case "Escape": {
          e.preventDefault();
          navigate({ to: "/groups/$groupId", params: { groupId } });
          break;
        }
      }
    },
    [members, nav, isAdmin, groupId, maxCol, navigate, setRoleMutation, currentUser],
  );

  if (isLoading) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          Loading…
        </p>
      </div>
    );
  }

  if (members.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
          No members.
        </p>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      data-testid="members-list"
      className="flex-1 flex flex-col overflow-auto outline-none"
    >
      {members.map((member, rowIndex) => {
        const isSelf = member.user_id === currentUser?.id;
        const isRowFocused = nav.rowIndex === rowIndex;
        const showControls = isAdmin && !isSelf;

        return (
          <div
            key={member.user_id}
            data-testid={`member-row-${member.user_id}`}
            className="flex items-center px-4 py-2 gap-3 text-xs font-mono select-none"
            style={{
              background: isRowFocused ? "var(--c-active)" : undefined,
              borderLeft: isRowFocused
                ? "2px solid var(--c-accent)"
                : "2px solid transparent",
            }}
          >
            {/* Row cursor indicator */}
            <span
              className="w-3 flex-shrink-0 text-center"
              style={{ color: "var(--c-accent)" }}
            >
              {isRowFocused && nav.colIndex === 0 ? ">" : " "}
            </span>

            {/* Member name */}
            <span className="flex-1 truncate" style={{ color: "var(--c-text)" }}>
              {member.username ?? member.user_id}
              {isSelf && (
                <span className="ml-2" style={{ color: "var(--c-text-muted)" }}>
                  (you)
                </span>
              )}
            </span>

            {/* Role label — shown when current user is not admin, or for own row */}
            {!showControls && (
              <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
                {member.role}
              </span>
            )}

            {/* Admin toggle + kick — only for other members when current user is admin */}
            {showControls && (
              <div className="flex items-center gap-8">
                <Switch
                  id={`member-admin-toggle-${member.user_id}`}
                  label="admin"
                  checked={member.role === "admin"}
                  onChange={() => {
                    const newRole = member.role === "admin" ? "member" : "admin";
                    setRoleMutation.mutate({ groupId, userId: member.user_id, role: newRole });
                  }}
                />
                <Button
                  data-testid={`member-kick-${member.user_id}`}
                  variant="primary"
                  onClick={() =>
                    navigate({
                      to: "/groups/$groupId/members/$userId/kick",
                      params: { groupId, userId: member.user_id },
                    })
                  }
                >
                  kick
                </Button>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
};
